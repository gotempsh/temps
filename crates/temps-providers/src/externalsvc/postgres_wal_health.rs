// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Postgres WAL & archive health probe.
//!
//! Detects "silent disk-filler" conditions on a managed Postgres service:
//! stale replication slots pinning WAL, a failing/misconfigured
//! `archive_command`, and unbounded `pg_wal` growth. The probe is read-only,
//! runs on a single `tokio_postgres` connection, and returns a structured
//! snapshot that the background `ExternalServiceHealthMonitor` persists on
//! `external_services.health_metadata.postgres_wal` and the UI surfaces as warnings.
//!
//! Thresholds are hardcoded for now. They scale off `max_wal_size` so they
//! self-tune to whatever the operator configured — no per-service knobs to
//! maintain until users ask for them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_postgres::NoTls;
use utoipa::ToSchema;

/// Multiple of `max_wal_size` at which `pg_wal/` is considered bloated.
const WAL_BLOAT_RATIO: f64 = 3.0;

/// Same ratio gates the "stale slot" warning — a slot is only worth flagging
/// when the WAL it pins is large enough that recycling would actually free
/// meaningful space. Using `max_wal_size` keeps the threshold proportional
/// to the operator-chosen WAL budget.
const STALE_SLOT_RATIO: f64 = 3.0;

/// `archive_status/*.ready` count above which the archiver is considered
/// backed up. Each `.ready` file represents one un-shipped 16 MB WAL segment.
const ARCHIVE_BACKLOG_READY_COUNT: i64 = 100;

/// Oldest WAL segment age (seconds) past which we warn that recycling has
/// stalled, _independent_ of total size. A `pg_wal` that's small but old
/// usually means a slot pin we missed or a hung archiver.
const WAL_NOT_RECYCLED_AGE_SECS: i64 = 24 * 3600;

/// Connect + per-query timeout. The probe runs alongside the regular
/// `health_probe` so it must stay well under the 30s poll interval.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Slot-name prefixes the probe treats as legitimate even when inactive.
/// `pgautofailover_*` slots come and go as replicas re-attach.
const SYSTEM_SLOT_PREFIXES: &[&str] = &["pgautofailover_"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveMode {
    Off,
    On,
    Always,
    Unknown,
}

impl ArchiveMode {
    fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "on" => Self::On,
            "always" => Self::Always,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StaleSlot {
    pub slot_name: String,
    pub active: bool,
    pub retained_bytes: i64,
}

/// One actionable warning surfaced to the UI.
///
/// Each variant carries the data needed to render a remediation hint without
/// the frontend re-querying anything.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WalWarning {
    /// `pg_wal` is significantly larger than `max_wal_size`.
    WalBloat {
        pg_wal_bytes: i64,
        max_wal_size_bytes: i64,
        ratio: f64,
    },
    /// A replication slot is holding WAL it's not consuming.
    StaleSlot {
        slot_name: String,
        retained_bytes: i64,
        active: bool,
    },
    /// `archive_status/*.ready` count exceeds threshold — `archive_command`
    /// is either failing or running slower than WAL generation.
    ArchiveBacklog { ready_count: i64 },
    /// `archive_mode = on` but `archive_command` is empty / `/bin/true`.
    /// WAL accumulates forever waiting for a destination that never accepts.
    ArchiveModeWithoutCommand,
    /// Oldest WAL segment is older than `WAL_NOT_RECYCLED_AGE_SECS`.
    /// Independent signal: something is blocking recycling even if total
    /// size hasn't exploded yet.
    WalNotRecycled { oldest_age_secs: i64 },
}

impl WalWarning {
    /// Severity hint for the UI banner color. `Critical` triggers red,
    /// `Warning` triggers yellow.
    pub fn severity(&self) -> WalWarningSeverity {
        match self {
            Self::WalBloat { ratio, .. } if *ratio >= 10.0 => WalWarningSeverity::Critical,
            Self::WalBloat { .. } => WalWarningSeverity::Warning,
            Self::StaleSlot { .. } => WalWarningSeverity::Critical,
            Self::ArchiveBacklog { .. } => WalWarningSeverity::Warning,
            Self::ArchiveModeWithoutCommand => WalWarningSeverity::Warning,
            Self::WalNotRecycled { .. } => WalWarningSeverity::Warning,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WalWarningSeverity {
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PostgresWalHealth {
    /// When the snapshot was taken.
    #[schema(value_type = String, format = DateTime)]
    pub probed_at: DateTime<Utc>,
    /// Total size of files under `pg_wal/`, from `pg_ls_waldir()`.
    pub pg_wal_bytes: i64,
    /// `max_wal_size` setting in bytes (parsed from `pg_settings`).
    pub max_wal_size_bytes: i64,
    pub archive_mode: ArchiveMode,
    /// The literal `archive_command` setting. May be empty or `/bin/true`
    /// when archiving is effectively disabled despite `archive_mode = on`.
    pub archive_command: Option<String>,
    /// Number of `archive_status/*.ready` files — un-shipped WAL segments.
    pub archive_backlog: i64,
    pub archiver_failed_count: Option<i64>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub archiver_last_failed_at: Option<DateTime<Utc>>,
    pub stale_slots: Vec<StaleSlot>,
    /// Age of the oldest WAL file in `pg_wal/` (seconds).
    pub oldest_wal_age_secs: i64,
    /// Computed warnings, ordered by severity (critical first).
    pub warnings: Vec<WalWarning>,
}

impl PostgresWalHealth {
    /// True when any warning is present. The health monitor uses this to
    /// downgrade the service to `degraded`.
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Build a libpq connection string from a `ServiceConfig`'s parameters JSON.
///
/// Mirrors what `PostgresService::health_probe` does internally. Returns
/// `None` when the parameters don't deserialize — the caller treats that as
/// "skip the WAL probe" rather than an error.
pub fn build_conn_str(parameters: &serde_json::Value) -> Option<String> {
    let host = parameters.get("host")?.as_str()?;
    let port = parameters.get("port")?.as_str()?;
    let user = parameters.get("username")?.as_str()?;
    let password = parameters.get("password")?.as_str()?;
    let database = parameters.get("database")?.as_str()?;
    Some(format!(
        "host={} port={} user={} password={} dbname={} connect_timeout=3",
        host, port, user, password, database,
    ))
}

/// Run the probe against a Postgres instance using libpq connection params.
///
/// The caller owns the connection string (the existing `health_probe` path
/// already builds one). On any error we return `Ok(None)` rather than
/// surfacing the error — the WAL probe is best-effort observability, not a
/// liveness signal, and a failure here must not cascade into the service
/// being marked down.
pub async fn probe_wal_health(conn_str: &str) -> Option<PostgresWalHealth> {
    let connect = tokio::time::timeout(PROBE_TIMEOUT, tokio_postgres::connect(conn_str, NoTls))
        .await
        .ok()?
        .ok()?;
    let (client, connection) = connect;

    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });

    let result = tokio::time::timeout(PROBE_TIMEOUT, collect_snapshot(&client))
        .await
        .ok()
        .and_then(|r| r.ok());

    connection_task.abort();
    result
}

async fn collect_snapshot(
    client: &tokio_postgres::Client,
) -> Result<PostgresWalHealth, tokio_postgres::Error> {
    let (archive_mode, archive_command, max_wal_size_bytes) = fetch_settings(client).await?;
    let pg_wal_bytes = fetch_pg_wal_bytes(client).await.unwrap_or(0);
    let stale_slots = fetch_stale_slots(client, max_wal_size_bytes)
        .await
        .unwrap_or_default();
    let archive_backlog = fetch_archive_backlog(client).await.unwrap_or(0);
    let (archiver_failed_count, archiver_last_failed_at) =
        fetch_archiver_stats(client).await.unwrap_or((None, None));
    let oldest_wal_age_secs = fetch_oldest_wal_age(client).await.unwrap_or(0);

    let mut snapshot = PostgresWalHealth {
        probed_at: Utc::now(),
        pg_wal_bytes,
        max_wal_size_bytes,
        archive_mode,
        archive_command,
        archive_backlog,
        archiver_failed_count,
        archiver_last_failed_at,
        stale_slots,
        oldest_wal_age_secs,
        warnings: Vec::new(),
    };

    snapshot.warnings = compute_warnings(&snapshot);
    Ok(snapshot)
}

async fn fetch_settings(
    client: &tokio_postgres::Client,
) -> Result<(ArchiveMode, Option<String>, i64), tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT name, setting FROM pg_settings \
             WHERE name IN ('archive_mode', 'archive_command', 'max_wal_size')",
            &[],
        )
        .await?;

    let mut archive_mode = ArchiveMode::Unknown;
    let mut archive_command: Option<String> = None;
    // max_wal_size in pg_settings is reported as an integer count of the unit
    // exposed by `unit` (typically MB on modern Postgres). Falling back to a
    // safe default of 1 GiB keeps the ratio math meaningful when the setting
    // can't be parsed for any reason.
    let mut max_wal_size_bytes: i64 = 1024 * 1024 * 1024;

    for row in rows {
        let name: String = row.get(0);
        let setting: String = row.get(1);
        match name.as_str() {
            "archive_mode" => archive_mode = ArchiveMode::parse(&setting),
            "archive_command" => {
                // pg_settings reports an unset archive_command as the literal
                // string "(disabled)" on some Postgres builds (notably 18+).
                // Normalize that — alongside an actually-empty value — to
                // None so downstream warning logic doesn't get confused.
                let trimmed = setting.trim();
                archive_command =
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("(disabled)") {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
            }
            "max_wal_size" => {
                // Postgres reports as integer-with-unit-MB on modern versions.
                if let Ok(mb) = setting.parse::<i64>() {
                    max_wal_size_bytes = mb.saturating_mul(1024 * 1024);
                }
            }
            _ => {}
        }
    }

    Ok((archive_mode, archive_command, max_wal_size_bytes))
}

async fn fetch_pg_wal_bytes(client: &tokio_postgres::Client) -> Result<i64, tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT COALESCE(SUM(size), 0)::bigint FROM pg_ls_waldir()",
            &[],
        )
        .await?;
    Ok(row.get::<_, i64>(0))
}

async fn fetch_stale_slots(
    client: &tokio_postgres::Client,
    max_wal_size_bytes: i64,
) -> Result<Vec<StaleSlot>, tokio_postgres::Error> {
    let threshold = ((max_wal_size_bytes as f64) * STALE_SLOT_RATIO) as i64;
    let rows = client
        .query(
            "SELECT slot_name, active, \
                COALESCE(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn), 0)::bigint \
                AS retained_bytes \
             FROM pg_replication_slots \
             WHERE restart_lsn IS NOT NULL",
            &[],
        )
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let slot_name: String = row.get(0);
            let active: bool = row.get(1);
            let retained_bytes: i64 = row.get(2);

            if SYSTEM_SLOT_PREFIXES
                .iter()
                .any(|p| slot_name.starts_with(p))
            {
                return None;
            }
            if active {
                return None;
            }
            if retained_bytes < threshold {
                return None;
            }
            Some(StaleSlot {
                slot_name,
                active,
                retained_bytes,
            })
        })
        .collect())
}

async fn fetch_archive_backlog(
    client: &tokio_postgres::Client,
) -> Result<i64, tokio_postgres::Error> {
    // pg_ls_dir on archive_status; .ready files = un-shipped segments.
    // Returns 0 if archiving is disabled (directory may not exist) — we
    // swallow the error in the caller.
    let row = client
        .query_one(
            "SELECT COUNT(*)::bigint FROM pg_ls_dir('pg_wal/archive_status') AS f \
             WHERE f LIKE '%.ready'",
            &[],
        )
        .await?;
    Ok(row.get::<_, i64>(0))
}

async fn fetch_archiver_stats(
    client: &tokio_postgres::Client,
) -> Result<(Option<i64>, Option<DateTime<Utc>>), tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT failed_count::bigint, last_failed_time FROM pg_stat_archiver",
            &[],
        )
        .await?;
    Ok((
        row.try_get::<_, i64>(0).ok(),
        row.try_get::<_, DateTime<Utc>>(1).ok(),
    ))
}

async fn fetch_oldest_wal_age(
    client: &tokio_postgres::Client,
) -> Result<i64, tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT COALESCE(EXTRACT(EPOCH FROM (now() - MIN(modification))), 0)::bigint \
             FROM pg_ls_waldir()",
            &[],
        )
        .await?;
    Ok(row.get::<_, i64>(0))
}

fn compute_warnings(snapshot: &PostgresWalHealth) -> Vec<WalWarning> {
    let mut warnings = Vec::new();

    if snapshot.max_wal_size_bytes > 0 {
        let ratio = snapshot.pg_wal_bytes as f64 / snapshot.max_wal_size_bytes as f64;
        if ratio >= WAL_BLOAT_RATIO {
            warnings.push(WalWarning::WalBloat {
                pg_wal_bytes: snapshot.pg_wal_bytes,
                max_wal_size_bytes: snapshot.max_wal_size_bytes,
                ratio,
            });
        }
    }

    for slot in &snapshot.stale_slots {
        warnings.push(WalWarning::StaleSlot {
            slot_name: slot.slot_name.clone(),
            retained_bytes: slot.retained_bytes,
            active: slot.active,
        });
    }

    if snapshot.archive_backlog >= ARCHIVE_BACKLOG_READY_COUNT {
        warnings.push(WalWarning::ArchiveBacklog {
            ready_count: snapshot.archive_backlog,
        });
    }

    if matches!(snapshot.archive_mode, ArchiveMode::On | ArchiveMode::Always) {
        let cmd_is_noop = match snapshot.archive_command.as_deref() {
            None => true,
            Some(cmd) => {
                let lower = cmd.trim().to_ascii_lowercase();
                lower.is_empty() || lower == "/bin/true" || lower == "true"
            }
        };
        if cmd_is_noop {
            warnings.push(WalWarning::ArchiveModeWithoutCommand);
        }
    }

    // `pg_wal/` naturally settles at ~max_wal_size — that's the configured
    // steady state, not a problem. Postgres also recycles segments by
    // renaming them in place, so mtime alone can be arbitrarily old on a
    // perfectly healthy database. Only flag "not recycled" when the segment
    // is old AND pg_wal is meaningfully bloated (same ratio as WalBloat).
    if snapshot.max_wal_size_bytes > 0
        && snapshot.oldest_wal_age_secs >= WAL_NOT_RECYCLED_AGE_SECS
        && (snapshot.pg_wal_bytes as f64 / snapshot.max_wal_size_bytes as f64) >= WAL_BLOAT_RATIO
    {
        warnings.push(WalWarning::WalNotRecycled {
            oldest_age_secs: snapshot.oldest_wal_age_secs,
        });
    }

    // Sort: critical first, then warning.
    warnings.sort_by_key(|w| match w.severity() {
        WalWarningSeverity::Critical => 0,
        WalWarningSeverity::Warning => 1,
    });

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_snapshot() -> PostgresWalHealth {
        PostgresWalHealth {
            probed_at: Utc::now(),
            pg_wal_bytes: 0,
            max_wal_size_bytes: 1024 * 1024 * 1024, // 1 GiB
            archive_mode: ArchiveMode::Off,
            archive_command: None,
            archive_backlog: 0,
            archiver_failed_count: None,
            archiver_last_failed_at: None,
            stale_slots: Vec::new(),
            oldest_wal_age_secs: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn healthy_snapshot_produces_no_warnings() {
        let s = base_snapshot();
        assert!(compute_warnings(&s).is_empty());
    }

    #[test]
    fn wal_bloat_warning_fires_at_3x() {
        let mut s = base_snapshot();
        // 3.5 GiB pg_wal against 1 GiB max
        s.pg_wal_bytes = (3.5 * 1024.0 * 1024.0 * 1024.0) as i64;
        let warnings = compute_warnings(&s);
        assert!(matches!(warnings[0], WalWarning::WalBloat { .. }));
    }

    #[test]
    fn wal_bloat_below_threshold_does_not_fire() {
        let mut s = base_snapshot();
        // 2x is under the 3x trigger.
        s.pg_wal_bytes = 2 * 1024 * 1024 * 1024;
        assert!(compute_warnings(&s).is_empty());
    }

    #[test]
    fn stale_slot_warning_passes_through() {
        let mut s = base_snapshot();
        s.stale_slots = vec![StaleSlot {
            slot_name: "abandoned_replica".to_string(),
            active: false,
            retained_bytes: 50 * 1024 * 1024 * 1024,
        }];
        let warnings = compute_warnings(&s);
        assert!(matches!(
            warnings[0],
            WalWarning::StaleSlot {
                ref slot_name,
                ..
            } if slot_name == "abandoned_replica"
        ));
    }

    #[test]
    fn archive_mode_on_with_empty_command_warns() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = None;
        let warnings = compute_warnings(&s);
        assert!(warnings
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveModeWithoutCommand)));
    }

    #[test]
    fn archive_mode_on_with_bin_true_warns() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some("/bin/true".to_string());
        let warnings = compute_warnings(&s);
        assert!(warnings
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveModeWithoutCommand)));
    }

    #[test]
    fn archive_mode_on_with_real_command_does_not_warn() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some(". /walg.env && wal-g wal-push %p".to_string());
        let warnings = compute_warnings(&s);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveModeWithoutCommand)));
    }

    #[test]
    fn archive_backlog_warns_at_threshold() {
        let mut s = base_snapshot();
        s.archive_backlog = ARCHIVE_BACKLOG_READY_COUNT;
        let warnings = compute_warnings(&s);
        assert!(warnings
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveBacklog { .. })));
    }

    #[test]
    fn wal_not_recycled_requires_both_age_and_bloat() {
        let mut s = base_snapshot();
        s.oldest_wal_age_secs = WAL_NOT_RECYCLED_AGE_SECS + 100;

        // pg_wal far below max — should NOT trigger.
        s.pg_wal_bytes = 100 * 1024 * 1024;
        assert!(compute_warnings(&s)
            .iter()
            .all(|w| !matches!(w, WalWarning::WalNotRecycled { .. })));

        // pg_wal at the configured ceiling (or just over) is the normal
        // steady state — old mtimes here come from in-place recycling, not
        // stalled recycling. Must NOT trigger.
        s.pg_wal_bytes = s.max_wal_size_bytes + 369; // mirrors the false-positive seen in prod
        assert!(compute_warnings(&s)
            .iter()
            .all(|w| !matches!(w, WalWarning::WalNotRecycled { .. })));

        // Old AND >= WAL_BLOAT_RATIO of max_wal_size — triggers.
        s.pg_wal_bytes = (WAL_BLOAT_RATIO * s.max_wal_size_bytes as f64) as i64;
        assert!(compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::WalNotRecycled { .. })));
    }

    #[test]
    fn critical_warnings_sort_before_warnings() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = None; // Warning severity
        s.stale_slots = vec![StaleSlot {
            slot_name: "x".to_string(),
            active: false,
            retained_bytes: 10 * 1024 * 1024 * 1024,
        }]; // Critical severity
        let warnings = compute_warnings(&s);
        assert!(matches!(warnings[0], WalWarning::StaleSlot { .. }));
    }

    #[test]
    fn archive_mode_parse_roundtrip() {
        assert_eq!(ArchiveMode::parse("off"), ArchiveMode::Off);
        assert_eq!(ArchiveMode::parse("on"), ArchiveMode::On);
        assert_eq!(ArchiveMode::parse("always"), ArchiveMode::Always);
        assert_eq!(ArchiveMode::parse("garbage"), ArchiveMode::Unknown);
    }

    #[test]
    fn has_warnings_reflects_warnings_vec() {
        let mut s = base_snapshot();
        assert!(!s.has_warnings());
        s.warnings.push(WalWarning::ArchiveModeWithoutCommand);
        assert!(s.has_warnings());
    }
}
