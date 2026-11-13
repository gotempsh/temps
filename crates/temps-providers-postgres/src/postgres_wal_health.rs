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

/// A segment waiting this long for `archive_command` means archiving has
/// stalled even when no attempt has failed yet: WAL-G retrying an endpoint
/// it cannot reach holds a single attempt open without recording a failure.
/// The metrics collector uses the same value (`ARCHIVE_STALL_SECS`).
pub const ARCHIVE_STALL_SECS: i64 = 300;

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
    /// `archive_mode = on` and nothing stores WAL, with no `archive_library`
    /// either. With an empty `archive_command`, PostgreSQL keeps every
    /// segment waiting for a command and `pg_wal` grows; with a no-op such as
    /// `/bin/true`, every segment is counted as archived and recycled, and
    /// nothing is stored. `archive_command` on the snapshot tells them apart.
    ArchiveModeWithoutCommand,
    /// Archiving is on, but the monitoring role may not list
    /// `archive_status` (`pg_ls_archive_statusdir()` needs superuser or
    /// `pg_monitor`), so a stalled archiver that has reported no failure
    /// yet cannot be detected, and the waiting-segment count reads 0.
    ArchiveStallCheckUnavailable,
    /// `archive_command`'s most recent attempt failed and nothing has
    /// archived since: PostgreSQL keeps every segment from here on.
    ArchiveFailing {
        last_failed_wal: Option<String>,
        /// How long the oldest un-archived segment has waited, when
        /// archiving has stalled rather than (or as well as) failed.
        #[serde(default)]
        stalled_secs: Option<i64>,
        #[schema(value_type = Option<String>, format = DateTime)]
        last_failed_at: Option<DateTime<Utc>>,
        #[schema(value_type = Option<String>, format = DateTime)]
        last_archived_at: Option<DateTime<Utc>>,
    },
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
            Self::ArchiveFailing { .. } => WalWarningSeverity::Critical,
            Self::ArchiveModeWithoutCommand => WalWarningSeverity::Warning,
            Self::ArchiveStallCheckUnavailable => WalWarningSeverity::Warning,
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
    /// The `archive_library` setting (PostgreSQL 15+), when one archives WAL
    /// instead of `archive_command`.
    #[serde(default)]
    pub archive_library: Option<String>,
    /// Number of `archive_status/*.ready` files — un-shipped WAL segments.
    /// 0 when `archive_status_unreadable` is set.
    pub archive_backlog: i64,
    /// The role may not list `archive_status`: the backlog, the oldest
    /// waiting segment's age and stall detection are unknown.
    #[serde(default)]
    pub archive_status_unreadable: bool,
    pub archiver_failed_count: Option<i64>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub archiver_last_failed_at: Option<DateTime<Utc>>,
    /// Segments archived since the statistics were last reset.
    #[serde(default)]
    pub archiver_archived_count: Option<i64>,
    /// When a segment was last archived successfully.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub archiver_last_archived_at: Option<DateTime<Utc>>,
    /// The segment whose archiving failed last.
    #[serde(default)]
    pub archiver_last_failed_wal: Option<String>,
    /// Age of the oldest `archive_status/*.ready` file (seconds), `None`
    /// when no segment is waiting.
    #[serde(default)]
    pub archive_oldest_ready_age_secs: Option<i64>,
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
/// `None` when a genuinely required field is missing — the caller treats
/// that as "skip the WAL probe" rather than an error.
pub fn build_conn_str(parameters: &serde_json::Value) -> Option<String> {
    // `host` is not user-editable (`PostgresInputConfig::host`'s
    // `#[serde(default = "default_host")]`) and is implicitly "localhost"
    // everywhere else this config is read — `PostgresService::health_probe`
    // gets it for free by deserializing through `PostgresInputConfig`. This
    // function instead reads the raw stored JSON directly, so requiring the
    // key like every other field silently broke it for standalone services
    // whose parameters simply never had an explicit "host" written: the `?`
    // returned `None` before a single connection was attempted, and `None`
    // from this function means "no WAL health snapshot", logged only at
    // debug level — i.e. permanently and silently disabled.
    let host = parameters
        .get("host")
        .and_then(|value| value.as_str())
        .unwrap_or("localhost");
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
    let (archive_mode, archive_command, archive_library, max_wal_size_bytes) =
        fetch_settings(client).await?;
    let pg_wal_bytes = fetch_pg_wal_bytes(client).await.unwrap_or(0);
    let stale_slots = fetch_stale_slots(client, max_wal_size_bytes)
        .await
        .unwrap_or_default();
    let archive_status = fetch_archive_backlog(client).await;
    let archive_status_unreadable = archive_status.is_err();
    let (archive_backlog, archive_oldest_ready_age_secs) = archive_status.unwrap_or((0, None));
    let archiver = fetch_archiver_stats(client).await.unwrap_or_default();
    let oldest_wal_age_secs = fetch_oldest_wal_age(client).await.unwrap_or(0);

    let mut snapshot = PostgresWalHealth {
        probed_at: Utc::now(),
        pg_wal_bytes,
        max_wal_size_bytes,
        archive_mode,
        archive_command,
        archive_library,
        archive_backlog,
        archive_status_unreadable,
        archiver_failed_count: archiver.failed_count,
        archiver_last_failed_at: archiver.last_failed_at,
        archiver_archived_count: archiver.archived_count,
        archiver_last_archived_at: archiver.last_archived_at,
        archiver_last_failed_wal: archiver.last_failed_wal,
        archive_oldest_ready_age_secs,
        stale_slots,
        oldest_wal_age_secs,
        warnings: Vec::new(),
    };

    snapshot.warnings = compute_warnings(&snapshot);
    Ok(snapshot)
}

async fn fetch_settings(
    client: &tokio_postgres::Client,
) -> Result<(ArchiveMode, Option<String>, Option<String>, i64), tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT name, setting FROM pg_settings \
             WHERE name IN ('archive_mode', 'archive_command', 'archive_library', 'max_wal_size')",
            &[],
        )
        .await?;

    let mut archive_mode = ArchiveMode::Unknown;
    let mut archive_command: Option<String> = None;
    let mut archive_library: Option<String> = None;
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
            "archive_library" => {
                let trimmed = setting.trim();
                archive_library = (!trimmed.is_empty()).then(|| trimmed.to_string());
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

    Ok((
        archive_mode,
        archive_command,
        archive_library,
        max_wal_size_bytes,
    ))
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

/// `.ready` files in `archive_status` (un-shipped segments) and the age of
/// the oldest, in seconds.
async fn fetch_archive_backlog(
    client: &tokio_postgres::Client,
) -> Result<(i64, Option<i64>), tokio_postgres::Error> {
    // Errors if archiving is disabled (directory may not exist) or the role
    // may not list it; the caller swallows that.
    let row = client
        .query_one(
            "SELECT COUNT(*)::bigint, \
                    EXTRACT(EPOCH FROM now() - MIN(modification))::bigint \
             FROM pg_ls_archive_statusdir() WHERE name LIKE '%.ready'",
            &[],
        )
        .await?;
    Ok((row.try_get(0)?, row.try_get(1)?))
}

/// One row of `pg_stat_archiver`.
#[derive(Debug, Default)]
struct ArchiverStats {
    failed_count: Option<i64>,
    last_failed_at: Option<DateTime<Utc>>,
    archived_count: Option<i64>,
    last_archived_at: Option<DateTime<Utc>>,
    last_failed_wal: Option<String>,
}

async fn fetch_archiver_stats(
    client: &tokio_postgres::Client,
) -> Result<ArchiverStats, tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT failed_count::bigint, last_failed_time, archived_count::bigint, \
                    last_archived_time, last_failed_wal \
             FROM pg_stat_archiver",
            &[],
        )
        .await?;
    Ok(ArchiverStats {
        failed_count: row.try_get::<_, i64>(0).ok(),
        last_failed_at: row.try_get::<_, DateTime<Utc>>(1).ok(),
        archived_count: row.try_get::<_, i64>(2).ok(),
        last_archived_at: row.try_get::<_, DateTime<Utc>>(3).ok(),
        last_failed_wal: row.try_get::<_, String>(4).ok(),
    })
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

    let archiving_on = matches!(snapshot.archive_mode, ArchiveMode::On | ArchiveMode::Always);
    let failed_since_last_success = match (
        snapshot.archiver_last_failed_at,
        snapshot.archiver_last_archived_at,
    ) {
        (Some(failed), Some(archived)) => failed > archived,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let stalled_secs = snapshot
        .archive_oldest_ready_age_secs
        .filter(|age| *age >= ARCHIVE_STALL_SECS);
    if archiving_on && (failed_since_last_success || stalled_secs.is_some()) {
        warnings.push(WalWarning::ArchiveFailing {
            // Only an outstanding failure: during a stall after a recovered
            // failure, the old segment is not what is failing now.
            last_failed_wal: snapshot
                .archiver_last_failed_wal
                .clone()
                .filter(|_| failed_since_last_success),
            stalled_secs,
            last_failed_at: snapshot.archiver_last_failed_at,
            last_archived_at: snapshot.archiver_last_archived_at,
        });
    }

    if archiving_on && snapshot.archive_status_unreadable {
        warnings.push(WalWarning::ArchiveStallCheckUnavailable);
    }

    // An `archive_library` archives WAL itself; `archive_command` is then
    // unused and normally empty.
    let archives_through_library = snapshot
        .archive_library
        .as_deref()
        .is_some_and(|library| !library.trim().is_empty());
    if archiving_on && !archives_through_library {
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

    #[test]
    fn conn_str_defaults_to_localhost_when_host_is_absent() {
        // `host` is not user-editable and is never written into stored
        // parameters for the overwhelming majority of standalone services —
        // it's implicitly "localhost" (`PostgresInputConfig::host`'s serde
        // default). Before this fix, a raw `parameters.get("host")?` bailed
        // out here and no WAL health snapshot was ever produced for such a
        // service, silently.
        let parameters = serde_json::json!({
            "port": "5432",
            "username": "postgres",
            "password": "secret",
            "database": "postgres",
        });
        assert_eq!(
            build_conn_str(&parameters).as_deref(),
            Some("host=localhost port=5432 user=postgres password=secret dbname=postgres connect_timeout=3")
        );
    }

    #[test]
    fn conn_str_honors_an_explicit_host_when_present() {
        let parameters = serde_json::json!({
            "host": "10.0.0.5",
            "port": "5432",
            "username": "postgres",
            "password": "secret",
            "database": "postgres",
        });
        assert_eq!(
            build_conn_str(&parameters).as_deref(),
            Some("host=10.0.0.5 port=5432 user=postgres password=secret dbname=postgres connect_timeout=3")
        );
    }

    #[test]
    fn conn_str_still_requires_password() {
        // Unlike `host`, a missing password is not a value the rest of the
        // codebase has a safe implicit default for — `PostgresConfig::from`
        // would synthesize a random one, which is exactly wrong for a probe
        // that must match the actual running container. Confirm this stays
        // a hard requirement rather than silently drifting the same way.
        let parameters = serde_json::json!({
            "host": "localhost",
            "port": "5432",
            "username": "postgres",
            "database": "postgres",
        });
        assert_eq!(build_conn_str(&parameters), None);
    }

    fn base_snapshot() -> PostgresWalHealth {
        PostgresWalHealth {
            probed_at: Utc::now(),
            pg_wal_bytes: 0,
            max_wal_size_bytes: 1024 * 1024 * 1024, // 1 GiB
            archive_mode: ArchiveMode::Off,
            archive_command: None,
            archive_backlog: 0,
            archive_library: None,
            archive_status_unreadable: false,
            archiver_failed_count: None,
            archiver_last_failed_at: None,
            archiver_archived_count: None,
            archiver_last_archived_at: None,
            archiver_last_failed_wal: None,
            archive_oldest_ready_age_secs: None,
            stale_slots: Vec::new(),
            oldest_wal_age_secs: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn archiving_that_failed_after_its_last_success_warns() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some(". /var/lib/postgresql/walg.env && wal-g wal-push %p".into());
        s.archiver_last_archived_at = Some(Utc::now() - chrono::Duration::hours(2));
        s.archiver_last_failed_at = Some(Utc::now());
        s.archiver_last_failed_wal = Some("000000010000000000000042".into());
        let warnings = compute_warnings(&s);
        assert!(matches!(
            &warnings[0],
            WalWarning::ArchiveFailing { last_failed_wal: Some(wal), .. }
                if wal == "000000010000000000000042"
        ));
        assert_eq!(warnings[0].severity(), WalWarningSeverity::Critical);
    }

    #[test]
    fn archiving_that_never_succeeded_warns() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some("wal-g wal-push %p".into());
        s.archiver_last_failed_at = Some(Utc::now());
        assert!(compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveFailing { .. })));
    }

    /// A stall after an earlier, recovered failure names no failed segment:
    /// the old one is not what is failing now.
    #[test]
    fn a_stall_after_a_recovered_failure_names_no_failed_segment() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some("wal-g wal-push %p".into());
        s.archiver_last_failed_at = Some(Utc::now() - chrono::Duration::hours(3));
        s.archiver_last_failed_wal = Some("000000010000000000000007".into());
        s.archiver_last_archived_at = Some(Utc::now() - chrono::Duration::hours(1));
        s.archive_oldest_ready_age_secs = Some(ARCHIVE_STALL_SECS + 60);
        let warnings = compute_warnings(&s);
        assert!(
            warnings.iter().any(|w| matches!(
                w,
                WalWarning::ArchiveFailing {
                    last_failed_wal: None,
                    stalled_secs: Some(_),
                    ..
                }
            )),
            "{warnings:?}"
        );
    }

    /// A role that may not list `archive_status` cannot see a stall: that
    /// is reported, never read as healthy archiving.
    #[test]
    fn an_unreadable_archive_status_says_stalls_cannot_be_seen() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some("wal-g wal-push %p".into());
        s.archive_status_unreadable = true;
        let warnings = compute_warnings(&s);
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, WalWarning::ArchiveStallCheckUnavailable)),
            "{warnings:?}"
        );
        assert_eq!(
            WalWarning::ArchiveStallCheckUnavailable.severity(),
            WalWarningSeverity::Warning
        );
        // With archiving off there is nothing to stall.
        s.archive_mode = ArchiveMode::Off;
        assert!(!compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveStallCheckUnavailable)));
    }

    /// An `archive_library` archives WAL with `archive_command` empty: that
    /// is archiving, not a missing command.
    #[test]
    fn an_archive_library_is_not_a_missing_command() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = None;
        s.archive_library = Some("basic_archive".into());
        assert!(!compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveModeWithoutCommand)));
        s.archive_library = None;
        assert!(compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveModeWithoutCommand)));
    }

    /// WAL-G retrying an endpoint it cannot reach never records a failure;
    /// segments just wait. Five minutes of that is failing archiving.
    #[test]
    fn a_segment_waiting_five_minutes_is_failing_archiving() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some("wal-g wal-push %p".into());
        s.archiver_last_archived_at = Some(Utc::now() - chrono::Duration::minutes(20));
        s.archive_backlog = 5;
        s.archive_oldest_ready_age_secs = Some(ARCHIVE_STALL_SECS + 60);
        let warnings = compute_warnings(&s);
        assert!(matches!(
            &warnings[0],
            WalWarning::ArchiveFailing { stalled_secs: Some(age), last_failed_wal: None, .. }
                if *age == ARCHIVE_STALL_SECS + 60
        ));

        // Queued for seconds, as on any busy server: no warning.
        s.archive_oldest_ready_age_secs = Some(5);
        assert!(!compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveFailing { .. })));
    }

    #[test]
    fn a_success_after_a_failure_clears_the_archiving_warning() {
        let mut s = base_snapshot();
        s.archive_mode = ArchiveMode::On;
        s.archive_command = Some("wal-g wal-push %p".into());
        s.archiver_last_failed_at = Some(Utc::now() - chrono::Duration::minutes(5));
        s.archiver_last_archived_at = Some(Utc::now());
        assert!(!compute_warnings(&s)
            .iter()
            .any(|w| matches!(w, WalWarning::ArchiveFailing { .. })));
        // Archiving off: old failures are history, not a warning.
        s.archive_mode = ArchiveMode::Off;
        s.archiver_last_archived_at = None;
        assert!(compute_warnings(&s).is_empty());
    }

    /// A snapshot persisted by an earlier version has none of the new
    /// archiver fields and must still load.
    #[test]
    fn a_snapshot_stored_before_the_archiver_fields_still_loads() {
        let stored = serde_json::json!({
            "probed_at": "2026-01-01T00:00:00Z",
            "pg_wal_bytes": 1,
            "max_wal_size_bytes": 2,
            "archive_mode": "on",
            "archive_command": null,
            "archive_backlog": 0,
            "archiver_failed_count": 3,
            "archiver_last_failed_at": null,
            "stale_slots": [],
            "oldest_wal_age_secs": 0,
            "warnings": []
        });
        let snapshot: PostgresWalHealth = serde_json::from_value(stored).unwrap();
        assert_eq!(snapshot.archiver_last_archived_at, None);
        assert_eq!(snapshot.archiver_last_failed_wal, None);
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
