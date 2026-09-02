// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps backfill cloud-telemetry` — re-send historical spans to Temps Cloud
//! at the project's current fidelity (ADR-040 §1).
//!
//! ```text
//! temps backfill cloud-telemetry --project <id> --from <ts> --to <ts> [--dry-run]
//! ```
//!
//! Raising `cloud_telemetry_fidelity` to `queryable` only changes spans
//! ingested *after* the change, which leaves a hole between "link established"
//! and "fidelity raised". This command fills it from whatever local storage
//! still holds.
//!
//! Structurally the sibling of `temps backfill clickhouse`: out of process,
//! cursor-based over `(start_time, id)`, batched, resumable, and safe to
//! re-run — Cloud dedupes on `submission_id` plus `(trace_id, span_id, ts)`.
//!
//! **`--dry-run` sends nothing.** It reports the exact row count, the estimated
//! metered bytes, and one fully projected example record, so "what am I about
//! to send and what will it cost" is answerable before the send rather than
//! after the invoice.
//!
//! # Run this with `temps serve` stopped
//!
//! Unlike the ClickHouse backfill, this one drives the *same* `CloudLink` state
//! file the running server uses for its live mirror. Two processes writing that
//! file will interleave submissions. Nothing is lost — Cloud's idempotency
//! covers it — but the run's own counters stop being trustworthy, so the
//! command says so up front.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use temps_audit::AuditService;
use temps_core::{AuditLogger, DBDateTime};
use temps_geo::{GeoIpService, IpAddressService, MockGeoIpService};
use temps_otel::services::cloud_backfill::{
    backfill_cloud_telemetry_window, estimate_backfill, CloudBackfillCursor, CloudBackfillSource,
    DEFAULT_BATCH_SIZE,
};
use temps_otel::services::cloud_backfill_audit::{
    record_backfill_audit, CloudTelemetryBackfillAudit,
};
use temps_otel::services::cloud_backfill_progress::CloudBackfillProgressService;
use temps_otel::services::cloud_fidelity::{CloudPolicyCache, CloudTelemetryPolicy};
use tracing::warn;

/// Progress-bar layout, matching the sibling `temps backfill` commands.
const PROGRESS_TEMPLATE: &str =
    "  {bar:40.cyan/blue} {pos:>10}/{len:<10} {percent:>3}%  ETA {eta_precise}  {msg}";

#[derive(Args, Clone)]
pub struct CloudTelemetryBackfillArgs {
    /// Project whose spans should be backfilled. Required — this command
    /// egresses data, so it never operates on "all projects" implicitly.
    #[arg(long)]
    pub project: i32,

    /// PostgreSQL connection URL (system of record for project settings, and
    /// the span source unless ClickHouse is configured).
    // hide_env_values: a Postgres URL embeds credentials.
    #[arg(long, env = "TEMPS_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Lower bound of the backfill window (RFC3339, inclusive).
    #[arg(long)]
    pub from: String,

    /// Upper bound of the backfill window (RFC3339, inclusive).
    #[arg(long)]
    pub to: String,

    /// Instance data directory holding `encryption_key` and the Cloud link
    /// state. Defaults to `$TEMPS_DATA_DIR`, then `~/.temps`.
    #[arg(long, env = "TEMPS_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Spans per Cloud submission. Capped at the transport's own batch size.
    #[arg(long, default_value_t = DEFAULT_BATCH_SIZE)]
    pub batch_size: u64,

    /// Slice the window into chunks this many days wide, so the resume
    /// checkpoint advances at a useful granularity on long windows.
    #[arg(long, default_value_t = 1u32)]
    pub chunk_days: u32,

    /// Optional throttle: stay under this many spans per second, so a backfill
    /// on a live instance does not monopolise local read IO or the Cloud
    /// ingest allowance.
    #[arg(long)]
    pub rate_limit_spans_per_sec: Option<u64>,

    /// Report what would be sent — row count, estimated metered bytes, and one
    /// fully projected example record — and send nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Persist a checkpoint after every chunk and resume from it on the next
    /// invocation with the same window.
    #[arg(long)]
    pub resume: bool,

    /// Where to persist the resume checkpoint. Defaults to
    /// `<data-dir>/cloud-telemetry-backfill.state`.
    #[arg(long)]
    pub state_file: Option<PathBuf>,

    /// ClickHouse HTTP endpoint. Set this (with the three below) when the OTel
    /// backend is ClickHouse — otherwise spans are read from the Postgres
    /// `otel_spans` table, which on a ClickHouse instance is empty.
    #[arg(long, env = "TEMPS_CLICKHOUSE_URL")]
    pub clickhouse_url: Option<String>,

    /// ClickHouse database holding the `spans` table.
    #[arg(long, env = "TEMPS_CLICKHOUSE_DATABASE")]
    pub clickhouse_database: Option<String>,

    /// ClickHouse username.
    #[arg(long, env = "TEMPS_CLICKHOUSE_USER")]
    pub clickhouse_user: Option<String>,

    /// ClickHouse password.
    #[arg(long, env = "TEMPS_CLICKHOUSE_PASSWORD", hide_env_values = true)]
    pub clickhouse_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CheckpointFile {
    last_start_time: Option<String>,
    last_row_id: Option<i64>,
    last_span_id: Option<String>,
    window_from: Option<String>,
    window_to: Option<String>,
    project_id: Option<i32>,
}

pub fn run(args: CloudTelemetryBackfillArgs) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(args))
}

async fn run_async(args: CloudTelemetryBackfillArgs) -> anyhow::Result<()> {
    print_header();

    let (from, to) = parse_window(&args)?;
    let data_dir = resolve_data_dir(args.data_dir.clone())?;

    let db = temps_database::establish_connection(&args.database_url).await?;
    let link = load_cloud_link(&data_dir)?;
    let source = build_source(&args, db.clone())?;

    // `resolve_project` rather than `policy_for`: the latter answers `Metered`
    // for a project that does not exist, which would send the operator off to
    // raise a fidelity setting on a project id they mistyped.
    let policy = CloudPolicyCache::new(db.clone())
        .resolve_project(args.project)
        .await?;

    print_plan(&args, &source, &policy, from, to);

    // Always estimate first: it is the `--dry-run` answer, the progress bar's
    // denominator, and — because it refuses on the same conditions a real run
    // does — the earliest point a misconfiguration surfaces.
    let estimate = estimate_backfill(&source, &link, &policy, args.project, from, to).await?;
    print_estimate(&estimate);

    if args.dry_run {
        print_example_record(&source, &link, &policy, args.project, from, to).await?;
        println!();
        println!(
            "{} {}",
            "✓".bright_green(),
            "Dry run — nothing was sent to Temps Cloud.".bright_white()
        );
        return Ok(());
    }

    if estimate.spans == 0 {
        println!(
            "{} No local spans in this window — nothing to backfill.",
            "✓".bright_green()
        );
        return Ok(());
    }

    println!(
        "{} {}",
        "!".bright_yellow(),
        "This drives the same Cloud link state file `temps serve` uses. Stop the \
         server first, or expect this run's counters to interleave with the live mirror's."
            .bright_yellow()
    );
    println!();

    let state_path = args
        .state_file
        .clone()
        .unwrap_or_else(|| data_dir.join("cloud-telemetry-backfill.state"));
    let mut cursor = load_checkpoint_or_default(&args, &state_path, from, to);
    if cursor != CloudBackfillCursor::default() {
        println!(
            "{} Resuming from checkpoint at {}",
            "→".bright_blue(),
            cursor
                .last_start_time
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_else(|| "the start of the window".to_string())
                .bright_cyan()
        );
    }

    let rate_limit = args.rate_limit_spans_per_sec.map(|per_second| {
        let seconds = args.batch_size as f64 / per_second.max(1) as f64;
        Duration::from_millis((seconds * 1000.0) as u64)
    });

    let progress = ProgressBar::new(estimate.spans);
    progress.set_style(progress_style());

    // The Console's half of the progress story. The local checkpoint above is
    // private to this terminal; this row is what `temps serve` and the UI can
    // see, so an operator who did not type this command can still tell whether
    // a backfill is running, stalled or done (CLAUDE.md: a feature the user
    // cannot find does not exist).
    let shared = CloudBackfillProgressService::new(db.clone());
    report_progress(
        &shared.start(args.project, estimate.spans, from, to).await,
        "start",
    );

    // The permanent half. The progress row above is `UNIQUE (project_id)` and
    // is overwritten by the next run; these entries accumulate, so "did we ship
    // this project's real span data, which window, and when" stays answerable
    // after a re-run. Written before the first span is offered.
    let audit = audit_logger(db.clone());
    let audit_event = CloudTelemetryBackfillAudit::started(
        args.project,
        from,
        to,
        policy.fidelity,
        policy.attribute_allowlist.len(),
        source.describe(),
        estimate.spans,
    );
    record_backfill_audit(audit.as_ref(), &audit_event).await;

    let chunks = split_window(from, to, args.chunk_days);
    let mut shipped_total = 0u64;
    let mut bytes_total = 0u64;

    for (index, (chunk_from, chunk_to)) in chunks.iter().copied().enumerate() {
        progress.set_message(format!(
            "chunk {}/{} ({}…{})",
            index + 1,
            chunks.len(),
            chunk_from.format("%Y-%m-%d"),
            chunk_to.format("%Y-%m-%d"),
        ));

        // Chunks entirely behind the cursor were completed by a previous run.
        if let Some(last) = cursor.last_start_time {
            if chunk_to < last {
                continue;
            }
        }

        let already_shipped = shipped_total;
        let report = match backfill_cloud_telemetry_window(
            &source,
            &link,
            &policy,
            args.project,
            chunk_from,
            chunk_to,
            args.batch_size,
            cursor.clone(),
            rate_limit,
            |running| progress.set_position(already_shipped + running.spans_shipped),
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                // Leave the shared record in a state that explains itself. A
                // `running` row frozen forever is exactly the "spinner that
                // never resolves" this feature exists to avoid.
                progress.abandon_with_message("failed");
                report_progress(
                    &shared
                        .fail(
                            args.project,
                            shipped_total,
                            estimate.spans,
                            error.to_string(),
                        )
                        .await,
                    "failure",
                );
                // Whatever already shipped is gone and billed, so the audit
                // trail records the partial figure rather than nothing.
                record_backfill_audit(
                    audit.as_ref(),
                    &audit_event.failed(shipped_total, bytes_total, error.to_string()),
                )
                .await;
                return Err(error.into());
            }
        };

        cursor = report.final_cursor.clone();
        shipped_total += report.spans_shipped;
        bytes_total = bytes_total.saturating_add(report.estimated_metered_bytes);
        progress.set_position(shipped_total);

        // Same cadence as the local checkpoint: one cheap metadata UPDATE per
        // chunk, not per batch.
        report_progress(
            &shared
                .record_progress(args.project, shipped_total, estimate.spans)
                .await,
            "progress",
        );

        if args.resume {
            persist_checkpoint(&state_path, &args, from, to, &cursor)?;
        }
    }

    progress.finish_with_message("done");
    report_progress(
        &shared
            .complete(args.project, shipped_total, estimate.spans)
            .await,
        "completion",
    );
    record_backfill_audit(
        audit.as_ref(),
        &audit_event.succeeded(shipped_total, bytes_total),
    )
    .await;

    println!();
    println!(
        "{} Backfill complete: {} span(s) accepted by Temps Cloud",
        "✓".bright_green(),
        shipped_total.to_string().bright_white().bold(),
    );
    println!(
        "    Estimated metered bytes: {}",
        format_bytes(bytes_total).bright_cyan()
    );
    println!(
        "    {}",
        "Temps Cloud's own acknowledgement is the authoritative billing figure.".bright_black()
    );

    if args.resume {
        // A clean end-to-end run: clear the checkpoint so the next invocation
        // against a different window does not resume into it.
        let _ = fs::remove_file(&state_path);
    }

    Ok(())
}

/// Log — never propagate — a failed write to the shared progress record.
///
/// The record is bookkeeping *about* a data transfer, not part of it. Aborting
/// a paid, half-finished egress because a status field would not persist is
/// strictly worse than losing the status field, and the terminal's own progress
/// bar is unaffected either way. The warning is loud enough that an operator
/// who then finds the Console stale knows why.
fn report_progress<T>(
    outcome: &Result<T, temps_otel::services::CloudBackfillProgressError>,
    stage: &str,
) {
    if let Err(error) = outcome {
        warn!(
            stage,
            error = %error,
            "Could not update the shared backfill progress record; the transfer is \
             unaffected but the Console will show stale progress for this run"
        );
    }
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

/// The styled bar, degrading to indicatif's default if the template ever fails
/// to compile.
///
/// [`PROGRESS_TEMPLATE`] is a constant and a test pins that it parses, so the
/// fallback is unreachable in practice — but an `expect` here would abort a
/// paid, in-flight egress over a cosmetic detail, and this repo does not permit
/// `expect` in a production path regardless of how unreachable it looks.
fn progress_style() -> ProgressStyle {
    match ProgressStyle::with_template(PROGRESS_TEMPLATE) {
        Ok(style) => style.progress_chars("█▓░"),
        Err(error) => {
            warn!(
                %error,
                "Could not build the progress bar style; falling back to the default bar"
            );
            ProgressStyle::default_bar()
        }
    }
}

/// The audit sink for this run.
///
/// `AuditService` takes an `IpAddressService` because request-borne events
/// resolve a client IP. A backfill has no request and no client IP — its audit
/// entries report `ip_address: None` — so the geolocation dependency is never
/// consulted, and wiring the real MaxMind reader here would read a database
/// into a one-shot process to serve exactly zero lookups.
fn audit_logger(db: Arc<sea_orm::DatabaseConnection>) -> Arc<dyn AuditLogger> {
    let geoip = Arc::new(GeoIpService::Mock(MockGeoIpService::new()));
    let ip_service = Arc::new(IpAddressService::new(db.clone(), geoip));
    Arc::new(AuditService::new(db, ip_service))
}

fn parse_window(args: &CloudTelemetryBackfillArgs) -> anyhow::Result<(DBDateTime, DBDateTime)> {
    let from = chrono::DateTime::parse_from_rfc3339(&args.from)
        .map_err(|e| anyhow::anyhow!("--from is not a valid RFC3339 timestamp: {e}"))?
        .with_timezone(&chrono::Utc);
    let to = chrono::DateTime::parse_from_rfc3339(&args.to)
        .map_err(|e| anyhow::anyhow!("--to is not a valid RFC3339 timestamp: {e}"))?
        .with_timezone(&chrono::Utc);
    if from > to {
        anyhow::bail!(
            "--from ({}) is after --to ({}); refusing to backfill an empty window",
            from.to_rfc3339(),
            to.to_rfc3339()
        );
    }
    Ok((from, to))
}

fn resolve_data_dir(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = explicit {
        return Ok(dir);
    }
    dirs::home_dir()
        .map(|home| home.join(".temps"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine the instance data directory. Pass --data-dir \
                 or set TEMPS_DATA_DIR."
            )
        })
}

/// Load the same link state the server uses, decrypted with the same key.
fn load_cloud_link(data_dir: &Path) -> anyhow::Result<Arc<temps_cloud_client::CloudLink>> {
    let key_path = data_dir.join("encryption_key");
    let key = fs::read_to_string(&key_path).map_err(|e| {
        anyhow::anyhow!(
            "Could not read the instance encryption key at {}: {e}. \
             This command must run on the instance's own machine, with \
             --data-dir (or TEMPS_DATA_DIR) pointing at its data directory.",
            key_path.display()
        )
    })?;
    let encryption = Arc::new(temps_core::EncryptionService::new(key.trim()).map_err(|e| {
        anyhow::anyhow!("Could not initialise the encryption service from {key_path:?}: {e}")
    })?);

    Ok(Arc::new(temps_cloud_client::CloudLink::load_encrypted(
        data_dir.to_path_buf(),
        env!("CARGO_PKG_VERSION"),
        encryption,
    )))
}

/// Pick the span source. ClickHouse when it is fully configured, Postgres
/// otherwise — and the choice is printed, so a zero-row run is explainable
/// rather than mysterious.
fn build_source(
    args: &CloudTelemetryBackfillArgs,
    db: Arc<sea_orm::DatabaseConnection>,
) -> anyhow::Result<CloudBackfillSource> {
    match (
        &args.clickhouse_url,
        &args.clickhouse_database,
        &args.clickhouse_user,
        &args.clickhouse_password,
    ) {
        (Some(url), Some(database), Some(user), Some(password)) => {
            let client = temps_otel::storage::clickhouse::ClickHouseOtelClient::new(
                temps_otel::storage::clickhouse::ClickHouseOtelConfig::new(
                    url.clone(),
                    database.clone(),
                    user.clone(),
                    password.clone(),
                ),
            );
            Ok(CloudBackfillSource::ClickHouse(Arc::new(
                client.client().clone(),
            )))
        }
        (None, None, None, None) => Ok(CloudBackfillSource::Timescale(db)),
        _ => anyhow::bail!(
            "ClickHouse is only partially configured. Provide all four of \
             --clickhouse-url, --clickhouse-database, --clickhouse-user and \
             --clickhouse-password (or none of them, to read spans from the \
             PostgreSQL `otel_spans` table)."
        ),
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn print_header() {
    println!();
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_blue()
    );
    println!(
        "{}",
        "   Temps Cloud telemetry backfill".bright_white().bold()
    );
    println!(
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_blue()
    );
    println!();
}

fn print_plan(
    args: &CloudTelemetryBackfillArgs,
    source: &CloudBackfillSource,
    policy: &CloudTelemetryPolicy,
    from: DBDateTime,
    to: DBDateTime,
) {
    println!("{} Project: {}", "→".bright_blue(), args.project);
    println!(
        "{} Window: {} → {}",
        "→".bright_blue(),
        from.to_rfc3339().bright_cyan(),
        to.to_rfc3339().bright_cyan()
    );
    println!(
        "{} Reading from: {}",
        "→".bright_blue(),
        source.describe().bright_cyan()
    );
    println!(
        "{} Fidelity: {}",
        "→".bright_blue(),
        policy.fidelity.to_string().bright_cyan()
    );
    if policy.attribute_allowlist.is_empty() {
        println!(
            "{} Attribute allowlist: {} — no attribute values will leave this instance",
            "→".bright_blue(),
            "empty".bright_cyan()
        );
    } else {
        println!(
            "{} Attribute allowlist ({}): {}",
            "→".bright_blue(),
            policy.attribute_allowlist.len(),
            policy
                .attribute_allowlist
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
                .bright_cyan()
        );
    }
    println!();
}

fn print_estimate(estimate: &temps_otel::services::cloud_backfill::CloudBackfillEstimate) {
    println!(
        "{} Spans in window: {}",
        "→".bright_blue(),
        estimate.spans.to_string().bright_white().bold()
    );
    println!(
        "{} Estimated metered bytes: {}",
        "→".bright_blue(),
        format_bytes(estimate.estimated_metered_bytes)
            .bright_white()
            .bold()
    );
    println!(
        "    (mean {:.0} B/span over a {}-span sample; Temps Cloud's own \
         acknowledgement is authoritative)",
        estimate.average_span_bytes, estimate.sampled_spans
    );
}

/// Show the operator one real, fully projected record.
///
/// "What exactly am I sending" is not answerable from a byte count, and a
/// self-hosted operator has nobody to ask. This prints the actual bytes that
/// would leave, for a real span from the window.
async fn print_example_record(
    source: &CloudBackfillSource,
    link: &temps_cloud_client::CloudLink,
    policy: &CloudTelemetryPolicy,
    project_id: i32,
    from: DBDateTime,
    to: DBDateTime,
) -> anyhow::Result<()> {
    let example = temps_otel::services::cloud_backfill::project_example_span(
        source, link, policy, project_id, from, to,
    )
    .await?;
    let Some(record) = example else {
        return Ok(());
    };
    println!();
    println!(
        "{} One record exactly as it would be sent:",
        "→".bright_blue()
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&record)
            .unwrap_or_else(|e| format!("<could not render: {e}>"))
            .bright_black()
    );
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {} ({bytes} B)", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// Windowing and resume
// ---------------------------------------------------------------------------

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

fn load_checkpoint_or_default(
    args: &CloudTelemetryBackfillArgs,
    path: &Path,
    window_from: DBDateTime,
    window_to: DBDateTime,
) -> CloudBackfillCursor {
    if !args.resume || !path.exists() {
        return CloudBackfillCursor::default();
    }

    let file: CheckpointFile = match fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
    {
        Some(file) => file,
        None => {
            warn!(
                path = %path.display(),
                "checkpoint file unreadable; starting this backfill from the window start"
            );
            return CloudBackfillCursor::default();
        }
    };

    // A different window almost always means the operator changed
    // --from/--to/--project between runs; resuming into it would skip data.
    let same_window = file.window_from.as_deref() == Some(window_from.to_rfc3339().as_str())
        && file.window_to.as_deref() == Some(window_to.to_rfc3339().as_str())
        && file.project_id == Some(args.project);
    if !same_window {
        warn!(
            path = %path.display(),
            "checkpoint is for a different project/window; ignoring it and starting fresh"
        );
        return CloudBackfillCursor::default();
    }

    CloudBackfillCursor {
        last_start_time: file
            .last_start_time
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc)),
        last_row_id: file.last_row_id,
        last_span_id: file.last_span_id,
    }
}

fn persist_checkpoint(
    path: &Path,
    args: &CloudTelemetryBackfillArgs,
    window_from: DBDateTime,
    window_to: DBDateTime,
    cursor: &CloudBackfillCursor,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = CheckpointFile {
        last_start_time: cursor.last_start_time.map(|ts| ts.to_rfc3339()),
        last_row_id: cursor.last_row_id,
        last_span_id: cursor.last_span_id.clone(),
        window_from: Some(window_from.to_rfc3339()),
        window_to: Some(window_to.to_rfc3339()),
        project_id: Some(args.project),
    };
    fs::write(path, serde_json::to_string_pretty(&file)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Source selection never issues a query, so a disconnected handle is
    /// enough to exercise it.
    fn disconnected_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::DatabaseConnection::Disconnected)
    }

    fn args() -> CloudTelemetryBackfillArgs {
        CloudTelemetryBackfillArgs {
            project: 7,
            database_url: "postgres://localhost/temps".into(),
            from: "2026-08-01T00:00:00Z".into(),
            to: "2026-08-03T00:00:00Z".into(),
            data_dir: None,
            batch_size: DEFAULT_BATCH_SIZE,
            chunk_days: 1,
            rate_limit_spans_per_sec: None,
            dry_run: false,
            resume: false,
            state_file: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
        }
    }

    #[test]
    fn an_inverted_window_is_refused_before_anything_connects() {
        let mut args = args();
        std::mem::swap(&mut args.from, &mut args.to);

        let error = parse_window(&args).expect_err("an inverted window must be refused");

        assert!(error.to_string().contains("is after"), "{error}");
    }

    #[test]
    fn a_malformed_timestamp_names_the_flag_that_is_wrong() {
        let mut args = args();
        args.from = "yesterday".into();

        let error = parse_window(&args).expect_err("a malformed timestamp must be refused");

        assert!(error.to_string().contains("--from"), "{error}");
    }

    #[test]
    fn a_valid_window_parses_to_utc() {
        let (from, to) = parse_window(&args()).expect("window must parse");
        assert_eq!(from.to_rfc3339(), "2026-08-01T00:00:00+00:00");
        assert_eq!(to.to_rfc3339(), "2026-08-03T00:00:00+00:00");
    }

    #[test]
    fn partially_configured_clickhouse_is_refused_rather_than_silently_reading_postgres() {
        // Reading an empty `otel_spans` table on a ClickHouse instance and
        // reporting "0 spans" would look like success. Refuse instead.
        let mut args = args();
        args.clickhouse_url = Some("http://clickhouse:8123".into());

        let error = build_source(&args, disconnected_db())
            .expect_err("partial ClickHouse config must be refused");

        assert!(
            error.to_string().contains("--clickhouse-database"),
            "{error}"
        );
    }

    #[test]
    fn no_clickhouse_configuration_reads_the_postgres_span_table() {
        let source = build_source(&args(), disconnected_db()).expect("default source must build");
        assert_eq!(source.describe(), "PostgreSQL `otel_spans`");
    }

    #[test]
    fn fully_configured_clickhouse_reads_the_clickhouse_span_table() {
        let mut args = args();
        args.clickhouse_url = Some("http://clickhouse:8123".into());
        args.clickhouse_database = Some("temps".into());
        args.clickhouse_user = Some("default".into());
        args.clickhouse_password = Some("secret".into());

        let source = build_source(&args, disconnected_db()).expect("ClickHouse source must build");
        assert_eq!(source.describe(), "ClickHouse `spans`");
    }

    #[test]
    fn the_window_splits_into_chunks_that_cover_it_without_overlap() {
        let (from, to) = parse_window(&args()).expect("window must parse");
        let chunks = split_window(from, to, 1);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, from);
        assert_eq!(chunks.last().expect("non-empty").1, to);
        for pair in chunks.windows(2) {
            assert!(
                pair[0].1 < pair[1].0,
                "chunks must not overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_zero_length_window_still_yields_one_chunk() {
        let mut args = args();
        args.to = args.from.clone();
        let (from, to) = parse_window(&args).expect("window must parse");

        assert_eq!(split_window(from, to, 1), vec![(from, to)]);
    }

    #[test]
    fn a_checkpoint_for_a_different_project_is_ignored_rather_than_skipping_data() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("cloud-telemetry-backfill.state");
        let mut args = args();
        args.resume = true;
        let (from, to) = parse_window(&args).expect("window must parse");

        let cursor = CloudBackfillCursor {
            last_start_time: Some(from + chrono::Duration::hours(1)),
            last_row_id: Some(42),
            last_span_id: Some("00f067aa0ba902b7".into()),
        };
        persist_checkpoint(&path, &args, from, to, &cursor).expect("checkpoint must persist");

        // Same file, different project: must not resume.
        let mut other = args.clone();
        other.project = 8;
        assert_eq!(
            load_checkpoint_or_default(&other, &path, from, to),
            CloudBackfillCursor::default()
        );

        // Same project and window: resumes exactly where it stopped.
        assert_eq!(load_checkpoint_or_default(&args, &path, from, to), cursor);
    }

    #[test]
    fn resume_is_opt_in_so_a_plain_re_run_starts_from_the_window_start() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("cloud-telemetry-backfill.state");
        let mut with_resume = args();
        with_resume.resume = true;
        let (from, to) = parse_window(&with_resume).expect("window must parse");
        persist_checkpoint(
            &path,
            &with_resume,
            from,
            to,
            &CloudBackfillCursor {
                last_start_time: Some(from + chrono::Duration::hours(1)),
                last_row_id: Some(42),
                last_span_id: None,
            },
        )
        .expect("checkpoint must persist");

        assert_eq!(
            load_checkpoint_or_default(&args(), &path, from, to),
            CloudBackfillCursor::default()
        );
    }

    #[test]
    fn the_progress_template_parses_so_the_fallback_bar_is_never_used() {
        // `progress_style` degrades instead of panicking, which means a broken
        // template would silently ship a different-looking bar. This is what
        // catches the typo at build time instead.
        assert!(
            ProgressStyle::with_template(PROGRESS_TEMPLATE).is_ok(),
            "the progress template must parse; progress_style() would otherwise \
             fall back to the default bar"
        );
    }

    #[test]
    fn byte_counts_are_rendered_with_the_exact_figure_alongside_the_unit() {
        // An operator reconciling against an invoice needs the exact number,
        // not just "1.00 MB".
        assert_eq!(format_bytes(512), "512 B");
        assert!(format_bytes(1024).starts_with("1.00 KB"));
        assert!(format_bytes(1024).contains("(1024 B)"));
        assert!(format_bytes(5 * 1024 * 1024).contains("(5242880 B)"));
    }

    #[test]
    fn the_batch_size_default_matches_the_transport_submission_size() {
        // A larger batch than the link's own submission size would leave spans
        // sitting in the bounded producer channel, where they can be dropped.
        assert_eq!(args().batch_size, DEFAULT_BATCH_SIZE);
        assert_eq!(
            DEFAULT_BATCH_SIZE,
            temps_otel::services::cloud_backfill::MAX_BATCH_SIZE,
            "the default must equal the transport's own per-submission size; a \
             larger batch would leave spans sitting in the bounded producer \
             channel, where they are dropped rather than sent"
        );
    }
}
