//! TimescaleDB → ClickHouse backfill for the *backend-swap* observability
//! domains: proxy/request logs, OTel traces, and resource metrics.
//!
//! Unlike analytics `events` (which replicate live through the outbox + fan-out
//! worker), these three domains pick their backend at construction: when
//! `TEMPS_CLICKHOUSE_*` is set, new writes go to ClickHouse and the historical
//! rows already in TimescaleDB are left behind. This module copies that history
//! across so the UI shows a continuous record after a cutover.
//!
//! ## Design
//!
//! For each domain we:
//! 1. Ensure the ClickHouse schema exists (each domain's idempotent migration
//!    runner) — so the operator never has to enable `temps serve` first.
//! 2. Resolve the `[since, now]` window and count the source rows.
//! 3. Walk the source table in keyset order, reading raw rows in batches and
//!    inserting them into ClickHouse via the same `client.insert::<Row>(table)`
//!    → `write` → `end` API the live writers use.
//!
//! ## Idempotency
//!
//! Every ClickHouse table is a `ReplacingMergeTree(_version)`. We set `_version`
//! to the **source row's timestamp in milliseconds** (NOT `now()`, which the
//! live `From` impls use). That makes a re-run — or an overlapping `--since`
//! window — collapse to one row per logical record on merge, so the copy is
//! safe to repeat and resume by simply re-running with the same or a narrower
//! window.

use std::sync::Arc;

use anyhow::Context;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use sea_orm::{DatabaseBackend, DatabaseConnection, FromQueryResult, Statement};
use temps_core::DBDateTime;
use tracing::info;

use super::backfill::{BackfillDomain, ClickhouseBackfillArgs};

/// Rows read from / written to ClickHouse per insert. CH prefers large batches;
/// this also bounds the client-side buffer and the PG read page.
const DEFAULT_COPY_BATCH: u64 = 10_000;

/// Run the backfill for one backend-swap domain.
pub async fn run_domain_backfill(
    db: Arc<DatabaseConnection>,
    args: &ClickhouseBackfillArgs,
    domain: BackfillDomain,
) -> anyhow::Result<()> {
    let label = match domain {
        BackfillDomain::ProxyLogs => "proxy / request logs",
        BackfillDomain::Traces => "OTel traces (spans)",
        BackfillDomain::Metrics => "resource metrics",
        BackfillDomain::Events | BackfillDomain::All => {
            anyhow::bail!("run_domain_backfill is only for proxy-logs/traces/metrics")
        }
    };
    println!("{}", format!("▸ Domain: {label}").bright_white().bold());

    // Build a ClickHouse client from the same connection args the events path
    // uses. Construction does no I/O.
    let ch = ::clickhouse::Client::default()
        .with_url(&args.clickhouse_url)
        .with_database(&args.clickhouse_database)
        .with_user(&args.clickhouse_user)
        .with_password(&args.clickhouse_password);

    // 1. Ensure the target schema exists (idempotent). Always run it — a
    //    backfill against a database where `temps serve` never enabled CH would
    //    otherwise fail on a missing table.
    apply_schema(&ch, args, domain).await?;

    // 2. Resolve the window: [since, max(source timestamp)].
    let spec = domain_spec(domain);
    let since = match &args.from {
        Some(s) => parse_rfc3339(s).context("--from is not a valid RFC3339 timestamp")?,
        None => match min_timestamp(db.as_ref(), spec).await? {
            Some(ts) => ts,
            None => {
                println!(
                    "{} {} is empty in TimescaleDB — nothing to copy.",
                    "✓".bright_green(),
                    spec.source_table
                );
                return Ok(());
            }
        },
    };
    let until = match &args.to {
        Some(s) => parse_rfc3339(s).context("--to is not a valid RFC3339 timestamp")?,
        None => match max_timestamp(db.as_ref(), spec).await? {
            Some(ts) => ts,
            None => {
                println!(
                    "{} {} is empty in TimescaleDB — nothing to copy.",
                    "✓".bright_green(),
                    spec.source_table
                );
                return Ok(());
            }
        },
    };
    if since > until {
        anyhow::bail!(
            "--from ({}) is after --to ({}); refusing to copy an empty window",
            since.to_rfc3339(),
            until.to_rfc3339()
        );
    }

    // Warn up-front if the window reaches past the CH table's INSERT-time TTL
    // (which would silently drop the oldest rows). Does not block the run.
    warn_if_ttl_truncates(&ch, spec, since).await;

    let total = count_window(db.as_ref(), spec, since, until).await?;
    println!(
        "{} Window: {} → {}",
        "→".bright_blue(),
        since.to_rfc3339().bright_cyan(),
        until.to_rfc3339().bright_cyan(),
    );
    println!(
        "{} Rows to copy: {}",
        "→".bright_blue(),
        total.to_string().bright_white().bold(),
    );

    if total == 0 {
        println!("{} Nothing to copy in this window.", "✓".bright_green());
        return Ok(());
    }

    if args.dry_run {
        println!(
            "{} {}",
            "✓".bright_green(),
            "Dry run — no rows written to ClickHouse.".bright_white()
        );
        return Ok(());
    }

    let batch_size = if args.batch_size == 0 {
        DEFAULT_COPY_BATCH
    } else {
        args.batch_size
    };

    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "  {bar:40.cyan/blue} {pos:>10}/{len:<10} {percent:>3}%  ETA {eta_precise}  {msg}",
        )
        .expect("valid progress template")
        .progress_chars("█▓░"),
    );

    // 3. Keyset walk + copy. Dispatch to the per-domain copier.
    let copied = match domain {
        BackfillDomain::ProxyLogs => {
            proxy_logs::copy(db.as_ref(), &ch, since, until, batch_size, &pb).await?
        }
        BackfillDomain::Traces => {
            traces::copy(db.as_ref(), &ch, since, until, batch_size, &pb).await?
        }
        BackfillDomain::Metrics => {
            metrics::copy(db.as_ref(), &ch, since, until, batch_size, &pb).await?
        }
        _ => unreachable!(),
    };

    pb.finish_with_message("done");
    println!(
        "{} Copied {} row(s) into ClickHouse {}/{}.",
        "✓".bright_green(),
        copied.to_string().bright_white().bold(),
        args.clickhouse_url.bright_cyan(),
        args.clickhouse_database.bright_cyan(),
    );

    // Verify the rows actually landed (TTL-on-insert can silently drop the
    // oldest). Warns on a material shortfall; never fails the run.
    reconcile_after_copy(&ch, spec, since, until, copied).await;
    println!();

    Ok(())
}

/// Static facts about a domain's TimescaleDB source table needed for windowing.
#[derive(Clone, Copy)]
struct DomainSpec {
    /// TimescaleDB source table name.
    source_table: &'static str,
    /// The time column used for the window filter and ordering.
    time_column: &'static str,
    /// ClickHouse destination table name (may differ — e.g. otel_spans→spans).
    ch_table: &'static str,
    /// ClickHouse time column (for the post-copy reconciliation count).
    ch_time_column: &'static str,
}

fn domain_spec(domain: BackfillDomain) -> DomainSpec {
    match domain {
        BackfillDomain::ProxyLogs => DomainSpec {
            source_table: "proxy_logs",
            time_column: "timestamp",
            ch_table: "proxy_logs",
            ch_time_column: "timestamp",
        },
        BackfillDomain::Traces => DomainSpec {
            source_table: "otel_spans",
            time_column: "start_time",
            ch_table: "spans",
            ch_time_column: "start_time",
        },
        BackfillDomain::Metrics => DomainSpec {
            source_table: "service_metrics",
            time_column: "time",
            ch_table: "service_metrics",
            ch_time_column: "time",
        },
        BackfillDomain::Events | BackfillDomain::All => {
            unreachable!("domain_spec only covers backend-swap domains")
        }
    }
}

async fn apply_schema(
    ch: &::clickhouse::Client,
    args: &ClickhouseBackfillArgs,
    domain: BackfillDomain,
) -> anyhow::Result<()> {
    println!("{} Ensuring ClickHouse schema exists…", "→".bright_blue());
    let db_name = &args.clickhouse_database;
    match domain {
        BackfillDomain::ProxyLogs => {
            temps_proxy::storage::clickhouse_migrations::apply_migrations(ch, db_name)
                .await
                .map_err(|e| anyhow::anyhow!("proxy-log CH migrations failed: {e}"))?;
        }
        BackfillDomain::Traces => {
            temps_otel::storage::clickhouse::migrations::apply_migrations(ch, db_name)
                .await
                .map_err(|e| anyhow::anyhow!("otel CH migrations failed: {e}"))?;
        }
        BackfillDomain::Metrics => {
            temps_metrics::store::clickhouse_migrations::apply_migrations(ch, db_name)
                .await
                .map_err(|e| anyhow::anyhow!("metrics CH migrations failed: {e}"))?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Warn the operator up-front if the backfill window reaches past the
/// ClickHouse table's TTL.
///
/// ClickHouse enforces TTL **at INSERT time** — a row whose timestamp is older
/// than `now() - TTL` is silently discarded on insert, with no error. So
/// backfilling historical data older than the table's retention window would
/// look like a success while quietly dropping the oldest rows. We read the
/// table's `TTL ... INTERVAL N DAY` from `system.tables`, compute the cutoff,
/// and warn (with the exact `ALTER TABLE … MODIFY TTL` remediation) when the
/// requested `--from` predates it. We warn rather than abort so an operator who
/// has already widened the TTL, or who genuinely only wants recent data, isn't
/// blocked — but the post-copy reconciliation will also flag any actual drop.
async fn warn_if_ttl_truncates(ch: &::clickhouse::Client, spec: DomainSpec, since: DBDateTime) {
    #[derive(::clickhouse::Row, serde::Deserialize)]
    struct TtlRow {
        ttl_days: i64,
    }

    // Parse the TTL day count out of the table's create-statement metadata.
    // `engine_full` carries the `TTL toDateTime(<col>) + toIntervalDay(N)`
    // clause. extract the integer N; 0/absent means "no TTL".
    let sql = format!(
        "SELECT toInt64(ifNull( \
            extract(engine_full, 'toIntervalDay\\\\((\\\\d+)\\\\)'), '0')) AS ttl_days \
         FROM system.tables WHERE database = currentDatabase() AND name = '{}'",
        spec.ch_table
    );

    let ttl_days = match ch.query(&sql).fetch_one::<TtlRow>().await {
        Ok(r) => r.ttl_days,
        Err(_) => return, // best-effort; reconciliation still backstops us
    };
    if ttl_days <= 0 {
        return; // no TTL on this table
    }

    // Cutoff = now - ttl_days. Compare against the requested window start.
    // We compute it in SQL to avoid pulling `now()` into the (forbidden)
    // Rust clock; but a simple chrono comparison is fine here since this is a
    // warning, not a correctness gate.
    let cutoff = chrono::Utc::now() - chrono::Duration::days(ttl_days);
    if since < cutoff {
        println!();
        println!(
            "{} ClickHouse table `{}` has a {}-day TTL, enforced at INSERT.",
            "⚠".yellow().bold(),
            spec.ch_table,
            ttl_days,
        );
        println!(
            "  Rows older than {} will be SILENTLY DROPPED on insert.",
            cutoff.to_rfc3339().yellow(),
        );
        println!(
            "  Your window starts at {} — older rows in it won't land.",
            since.to_rfc3339().yellow(),
        );
        println!("  To import them, widen the TTL first, e.g.:");
        println!(
            "    {}",
            format!(
                "ALTER TABLE {} MODIFY TTL toDateTime({}) + INTERVAL <N> DAY;",
                spec.ch_table, spec.ch_time_column
            )
            .bright_cyan(),
        );
        println!();
    }
}

/// After a copy, compare how many rows actually landed in ClickHouse for the
/// window against how many we submitted, and warn on a shortfall — the
/// signature of TTL-on-insert silently dropping the oldest rows. Best-effort:
/// a query error here never fails the backfill.
///
/// Note this counts with `FINAL` so ReplacingMergeTree duplicates from a
/// re-run don't make the landed count look *larger* than submitted and mask a
/// real drop.
async fn reconcile_after_copy(
    ch: &::clickhouse::Client,
    spec: DomainSpec,
    since: DBDateTime,
    until: DBDateTime,
    submitted: u64,
) {
    #[derive(::clickhouse::Row, serde::Deserialize)]
    struct CntRow {
        cnt: u64,
    }
    let sql = format!(
        "SELECT count() AS cnt FROM {} FINAL \
         WHERE {} >= fromUnixTimestamp64Milli({}) \
           AND {} <= fromUnixTimestamp64Milli({})",
        spec.ch_table,
        spec.ch_time_column,
        since.timestamp_millis(),
        spec.ch_time_column,
        until.timestamp_millis(),
    );
    let landed = match ch.query(&sql).fetch_one::<CntRow>().await {
        Ok(r) => r.cnt,
        Err(_) => return,
    };

    // `landed` is deduplicated (FINAL) and the source may itself contain
    // duplicates that collapse, so landed < submitted is expected and benign.
    // We only shout when landed is dramatically smaller — a >5% shortfall is
    // the fingerprint of TTL dropping rows, not normal dedup.
    if submitted > 0 && landed < submitted.saturating_sub(submitted / 20) {
        println!(
            "{} Only {} of {} submitted rows are present in ClickHouse `{}` for this window.",
            "⚠".yellow().bold(),
            landed.to_string().yellow(),
            submitted,
            spec.ch_table,
        );
        println!(
            "  This usually means the table's INSERT-time TTL dropped the oldest rows. \
             Widen the TTL (see the warning above) and re-run."
        );
    }
}

// ── Shared window helpers (raw SQL; injection-safe — table/column come from a
//    fixed `DomainSpec`, never user input) ──────────────────────────────────

fn parse_rfc3339(s: &str) -> anyhow::Result<DBDateTime> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)?.with_timezone(&chrono::Utc))
}

#[derive(FromQueryResult)]
struct TsRow {
    ts: Option<DBDateTime>,
}

#[derive(FromQueryResult)]
struct CountRow {
    cnt: i64,
}

async fn min_timestamp(
    db: &DatabaseConnection,
    spec: DomainSpec,
) -> anyhow::Result<Option<DBDateTime>> {
    let sql = format!(
        "SELECT MIN({}) AS ts FROM {}",
        spec.time_column, spec.source_table
    );
    let row = TsRow::find_by_statement(Statement::from_string(DatabaseBackend::Postgres, sql))
        .one(db)
        .await?;
    Ok(row.and_then(|r| r.ts))
}

async fn max_timestamp(
    db: &DatabaseConnection,
    spec: DomainSpec,
) -> anyhow::Result<Option<DBDateTime>> {
    let sql = format!(
        "SELECT MAX({}) AS ts FROM {}",
        spec.time_column, spec.source_table
    );
    let row = TsRow::find_by_statement(Statement::from_string(DatabaseBackend::Postgres, sql))
        .one(db)
        .await?;
    Ok(row.and_then(|r| r.ts))
}

async fn count_window(
    db: &DatabaseConnection,
    spec: DomainSpec,
    since: DBDateTime,
    until: DBDateTime,
) -> anyhow::Result<u64> {
    let sql = format!(
        "SELECT COUNT(*)::bigint AS cnt FROM {} WHERE {} >= $1 AND {} <= $2",
        spec.source_table, spec.time_column, spec.time_column
    );
    let row = CountRow::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![since.into(), until.into()],
    ))
    .one(db)
    .await?;
    Ok(row.map(|r| r.cnt.max(0) as u64).unwrap_or(0))
}

/// Days since the Unix epoch for a ClickHouse `Date` column.
fn epoch_days(ts: DBDateTime) -> u16 {
    (ts.timestamp() / 86_400) as u16
}

// ── proxy_logs ───────────────────────────────────────────────────────────────

mod proxy_logs {
    use super::*;
    use temps_proxy::storage::clickhouse::ChProxyLogRow;

    /// One raw proxy_logs row read from TimescaleDB. jsonb columns are read as
    /// text via `::text` casts in the SELECT so they map straight to String.
    #[derive(FromQueryResult)]
    struct Row {
        id: i32,
        timestamp: DBDateTime,
        method: String,
        path: String,
        query_string: Option<String>,
        host: String,
        status_code: i16,
        response_time_ms: Option<i32>,
        request_source: String,
        is_system_request: bool,
        routing_status: String,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        session_id: Option<i32>,
        visitor_id: Option<i32>,
        container_id: Option<String>,
        upstream_host: Option<String>,
        error_message: Option<String>,
        client_ip: Option<String>,
        user_agent: Option<String>,
        referrer: Option<String>,
        request_id: String,
        ip_geolocation_id: Option<i32>,
        browser: Option<String>,
        browser_version: Option<String>,
        operating_system: Option<String>,
        device_type: Option<String>,
        is_bot: Option<bool>,
        bot_name: Option<String>,
        request_size_bytes: Option<i64>,
        response_size_bytes: Option<i64>,
        cache_status: Option<String>,
        request_headers: Option<String>,
        response_headers: Option<String>,
        trace_id: Option<String>,
        error_group_id: Option<i32>,
    }

    const SELECT: &str = "SELECT \
        id, timestamp, method, path, query_string, host, status_code, response_time_ms, \
        request_source, is_system_request, routing_status, project_id, environment_id, \
        deployment_id, session_id, visitor_id, container_id, upstream_host, error_message, \
        client_ip, user_agent, referrer, request_id, ip_geolocation_id, browser, browser_version, \
        operating_system, device_type, is_bot, bot_name, request_size_bytes, response_size_bytes, \
        cache_status, request_headers::text AS request_headers, response_headers::text AS response_headers, \
        trace_id, error_group_id \
        FROM proxy_logs \
        WHERE timestamp >= $1 AND timestamp <= $2 \
          AND (timestamp, id) > ($3, $4) \
        ORDER BY timestamp ASC, id ASC \
        LIMIT $5";

    fn to_ch(r: &Row) -> ChProxyLogRow {
        let ms = r.timestamp.timestamp_millis();
        ChProxyLogRow {
            timestamp: ms,
            method: r.method.clone(),
            path: r.path.clone(),
            query_string: r.query_string.clone().unwrap_or_default(),
            host: r.host.clone(),
            status_code: r.status_code,
            response_time_ms: r.response_time_ms,
            request_source: r.request_source.clone(),
            is_system_request: u8::from(r.is_system_request),
            routing_status: r.routing_status.clone(),
            project_id: r.project_id,
            environment_id: r.environment_id,
            deployment_id: r.deployment_id,
            session_id: r.session_id,
            visitor_id: r.visitor_id,
            container_id: r.container_id.clone().unwrap_or_default(),
            upstream_host: r.upstream_host.clone().unwrap_or_default(),
            error_message: r.error_message.clone().unwrap_or_default(),
            client_ip: r.client_ip.clone().unwrap_or_default(),
            user_agent: r.user_agent.clone().unwrap_or_default(),
            referrer: r.referrer.clone().unwrap_or_default(),
            request_id: r.request_id.clone(),
            ip_geolocation_id: r.ip_geolocation_id,
            browser: r.browser.clone().unwrap_or_default(),
            browser_version: r.browser_version.clone().unwrap_or_default(),
            operating_system: r.operating_system.clone().unwrap_or_default(),
            device_type: r.device_type.clone().unwrap_or_default(),
            is_bot: r.is_bot.map(u8::from),
            bot_name: r.bot_name.clone().unwrap_or_default(),
            request_size_bytes: r.request_size_bytes,
            response_size_bytes: r.response_size_bytes,
            cache_status: r.cache_status.clone().unwrap_or_default(),
            request_headers: r.request_headers.clone().unwrap_or_else(|| "{}".into()),
            response_headers: r.response_headers.clone().unwrap_or_else(|| "{}".into()),
            created_date: epoch_days(r.timestamp),
            trace_id: r.trace_id.clone().unwrap_or_default(),
            error_group_id: r.error_group_id,
            // Source timestamp ms as the dedup version — stable across re-runs.
            _version: ms as u64,
        }
    }

    pub async fn copy(
        db: &DatabaseConnection,
        ch: &::clickhouse::Client,
        since: DBDateTime,
        until: DBDateTime,
        batch_size: u64,
        pb: &ProgressBar,
    ) -> anyhow::Result<u64> {
        let mut last_ts = since - chrono::Duration::milliseconds(1);
        let mut last_id: i32 = i32::MIN;
        let mut copied = 0u64;

        loop {
            let rows = Row::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                SELECT,
                vec![
                    since.into(),
                    until.into(),
                    last_ts.into(),
                    last_id.into(),
                    (batch_size as i64).into(),
                ],
            ))
            .all(db)
            .await?;

            if rows.is_empty() {
                break;
            }

            let mut inserter = ch
                .insert::<ChProxyLogRow>("proxy_logs")
                .await
                .context("ClickHouse proxy_logs inserter setup")?;
            for r in &rows {
                inserter
                    .write(&to_ch(r))
                    .await
                    .context("ClickHouse proxy_logs write")?;
            }
            inserter.end().await.context("ClickHouse proxy_logs end")?;

            let last = rows.last().expect("non-empty");
            last_ts = last.timestamp;
            last_id = last.id;
            copied += rows.len() as u64;
            pb.set_position(copied);

            if (rows.len() as u64) < batch_size {
                break;
            }
        }

        info!(copied, "proxy_logs backfill complete");
        Ok(copied)
    }
}

// ── otel_spans → spans ───────────────────────────────────────────────────────

mod traces {
    use super::*;
    use temps_otel::storage::clickhouse::ChSpanRow;

    #[derive(FromQueryResult)]
    struct Row {
        id: i64,
        project_id: i32,
        deployment_id: Option<i32>,
        service_name: String,
        service_version: Option<String>,
        deployment_environment: Option<String>,
        trace_id: String,
        span_id: String,
        parent_span_id: Option<String>,
        name: String,
        kind: String,
        start_time: DBDateTime,
        end_time: DBDateTime,
        duration_ms: f64,
        status_code: String,
        status_message: Option<String>,
        attributes: Option<String>,
        events: Option<String>,
    }

    const SELECT: &str = "SELECT \
        id, project_id, deployment_id, service_name, service_version, deployment_environment, \
        trace_id, span_id, parent_span_id, name, kind, start_time, end_time, duration_ms, \
        status_code, status_message, attributes::text AS attributes, events::text AS events \
        FROM otel_spans \
        WHERE start_time >= $1 AND start_time <= $2 \
          AND (start_time, id) > ($3, $4) \
        ORDER BY start_time ASC, id ASC \
        LIMIT $5";

    fn to_ch(r: &Row) -> ChSpanRow {
        ChSpanRow {
            project_id: r.project_id,
            deployment_id: r.deployment_id,
            service_name: r.service_name.clone(),
            service_version: r.service_version.clone().unwrap_or_default(),
            deployment_environment: r.deployment_environment.clone().unwrap_or_default(),
            trace_id: r.trace_id.clone(),
            span_id: r.span_id.clone(),
            parent_span_id: r.parent_span_id.clone().unwrap_or_default(),
            name: r.name.clone(),
            kind: r.kind.clone(),
            start_time: r.start_time.timestamp_millis(),
            end_time: r.end_time.timestamp_millis(),
            duration_ms: r.duration_ms,
            status_code: r.status_code.clone(),
            status_message: r.status_message.clone().unwrap_or_default(),
            attributes: r.attributes.clone().unwrap_or_else(|| "{}".into()),
            events: r.events.clone().unwrap_or_else(|| "[]".into()),
            // Source start_time ms as the dedup version — stable across re-runs.
            _version: r.start_time.timestamp_millis() as u64,
        }
    }

    pub async fn copy(
        db: &DatabaseConnection,
        ch: &::clickhouse::Client,
        since: DBDateTime,
        until: DBDateTime,
        batch_size: u64,
        pb: &ProgressBar,
    ) -> anyhow::Result<u64> {
        let mut last_ts = since - chrono::Duration::milliseconds(1);
        let mut last_id: i64 = i64::MIN;
        let mut copied = 0u64;

        loop {
            let rows = Row::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                SELECT,
                vec![
                    since.into(),
                    until.into(),
                    last_ts.into(),
                    last_id.into(),
                    (batch_size as i64).into(),
                ],
            ))
            .all(db)
            .await?;

            if rows.is_empty() {
                break;
            }

            let mut inserter = ch
                .insert::<ChSpanRow>("spans")
                .await
                .context("ClickHouse spans inserter setup")?;
            for r in &rows {
                inserter
                    .write(&to_ch(r))
                    .await
                    .context("ClickHouse spans write")?;
            }
            inserter.end().await.context("ClickHouse spans end")?;

            let last = rows.last().expect("non-empty");
            last_ts = last.start_time;
            last_id = last.id;
            copied += rows.len() as u64;
            pb.set_position(copied);

            if (rows.len() as u64) < batch_size {
                break;
            }
        }

        info!(copied, "otel_spans backfill complete");
        Ok(copied)
    }
}

// ── service_metrics ──────────────────────────────────────────────────────────

mod metrics {
    use super::*;
    use temps_metrics::store::clickhouse::ChMetricRow;

    /// `service_metrics` is a TimescaleDB hypertable with NO unique key:
    /// - There is no `id` column.
    /// - `ctid` is unique only *within a chunk*, not across the parent table,
    ///   so it cannot order a cross-chunk walk.
    /// - The `(time, source_id, name)` triple is NOT unique either — a single
    ///   triple legitimately has many rows (e.g. 27 rows of
    ///   `rustfs_s3_operations_total` at one timestamp for one source).
    ///
    /// A keyset walk with a strict `>` on a non-unique key SKIPS every row that
    /// ties the cursor at a page boundary — silent data loss. So this domain
    /// pages with `OFFSET`/`LIMIT` over a fully-deterministic `ORDER BY`
    /// instead, which never skips a row. To keep `OFFSET` cheap (it would be
    /// O(n²) over the whole table), the caller slices the window into small
    /// time-chunks first; each `OFFSET` walk only ranges over one chunk's rows.
    ///
    /// Byte-identical duplicate rows (same values in every column) collapse in
    /// ClickHouse's `ReplacingMergeTree` over the same sort key + `_version`, so
    /// copying every in-window row is correct — the target holds one row per
    /// distinct record regardless.
    #[derive(FromQueryResult)]
    struct Row {
        time: DBDateTime,
        source_kind: String,
        source_id: i32,
        name: String,
        value: f64,
        engine: Option<String>,
        environment: Option<String>,
        node_id: Option<i32>,
        labels: Option<String>,
    }

    // NOTE: the TimescaleDB `service_metrics` table has no `kind` column — the
    // gauge/counter distinction is not persisted there (the live writer derives
    // it from the metric type at ingest). For backfilled history we leave `kind`
    // empty; the dashboards treat '' as "unspecified", same as any pre-`kind`
    // row. labels (jsonb) is read as text so it maps straight to String.
    //
    // ORDER BY includes every column so the ordering is total and stable across
    // pages even when `(time, source_id, name)` ties — required for OFFSET
    // paging to be lossless. `value` is ordered too; NULLs are forced last for a
    // deterministic position.
    const SELECT: &str = "SELECT \
        time, source_kind, source_id, name, value, \
        engine, environment, node_id, labels::text AS labels \
        FROM service_metrics \
        WHERE time >= $1 AND time < $2 \
        ORDER BY time ASC, source_id ASC, name ASC, value ASC, \
                 source_kind ASC, engine ASC NULLS LAST, environment ASC NULLS LAST, \
                 node_id ASC NULLS LAST, labels ASC NULLS LAST \
        OFFSET $3 LIMIT $4";

    fn to_ch(r: &Row) -> ChMetricRow {
        let ms = r.time.timestamp_millis();
        ChMetricRow {
            time: ms,
            source_kind: r.source_kind.clone(),
            source_id: r.source_id,
            name: r.name.clone(),
            value: r.value,
            // No `kind` column in TimescaleDB service_metrics — see SELECT note.
            kind: String::new(),
            engine: r.engine.clone().unwrap_or_default(),
            environment: r.environment.clone().unwrap_or_default(),
            node_id: r.node_id,
            labels: r.labels.clone().unwrap_or_else(|| "{}".into()),
            _version: ms as u64,
        }
    }

    /// Width of each OFFSET-paging time-slice. Keeping slices ~1h bounds the
    /// number of rows any single `OFFSET` ranges over, so paging stays cheap
    /// even though the table has no key to keyset on. Resource metrics are
    /// written at most every ~30s per (source, name), so an hour is a few
    /// thousand rows per active source — comfortably small for OFFSET.
    const METRICS_SLICE: chrono::Duration = chrono::Duration::hours(1);

    pub async fn copy(
        db: &DatabaseConnection,
        ch: &::clickhouse::Client,
        since: DBDateTime,
        until: DBDateTime,
        batch_size: u64,
        pb: &ProgressBar,
    ) -> anyhow::Result<u64> {
        let mut copied = 0u64;

        // Walk the window in half-open time-slices [slice_start, slice_end).
        // `until` is inclusive at the call boundary, so the last slice extends
        // one millisecond past it to capture the final timestamp.
        let mut slice_start = since;
        let hard_end = until + chrono::Duration::milliseconds(1);

        while slice_start < hard_end {
            let slice_end = std::cmp::min(slice_start + METRICS_SLICE, hard_end);

            // OFFSET-page within this slice over the deterministic ORDER BY.
            // OFFSET only ranges over this slice's rows, so it never degrades
            // into a whole-table O(n²) scan.
            let mut offset: u64 = 0;
            loop {
                let rows = Row::find_by_statement(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    SELECT,
                    vec![
                        slice_start.into(),
                        slice_end.into(),
                        (offset as i64).into(),
                        (batch_size as i64).into(),
                    ],
                ))
                .all(db)
                .await?;

                if rows.is_empty() {
                    break;
                }

                let mut inserter = ch
                    .insert::<ChMetricRow>("service_metrics")
                    .await
                    .context("ClickHouse service_metrics inserter setup")?;
                for r in &rows {
                    inserter
                        .write(&to_ch(r))
                        .await
                        .context("ClickHouse service_metrics write")?;
                }
                inserter
                    .end()
                    .await
                    .context("ClickHouse service_metrics end")?;

                let n = rows.len() as u64;
                offset += n;
                copied += n;
                pb.set_position(copied);

                if n < batch_size {
                    break;
                }
            }

            slice_start = slice_end;
        }

        info!(copied, "service_metrics backfill complete");
        Ok(copied)
    }
}
