// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory snapshots of every published status page.
//!
//! The public endpoint never touches the database: it looks the slug up in an
//! [`ArcSwap`]ed map and returns pre-serialized bytes. That makes three
//! properties structural rather than a matter of care:
//!
//! - **No query amplification.** Public traffic, including slug enumeration,
//!   costs one hash lookup. An unknown or unpublished slug is simply absent.
//! - **Immediate takedown.** Unpublish/delete remove the entry under the writer
//!   lock before the API call returns.
//! - **Nothing unselected can leak.** A snapshot is assembled only from the
//!   page's selected monitors and the public incident updates that concern
//!   them; there is no code path from a public request to any other row.
//!
//! Snapshots are rebuilt by a background loop every [`REFRESH_INTERVAL`] with a
//! fixed number of batched queries, independent of the number of pages, and
//! per page whenever an admin changes it. Completed UTC days of uptime history
//! are cached per monitor and recomputed once per day, so a refresh scans at
//! most the current day of raw checks.
//!
//! Load and bounds: one refresh is ~6 queries per minute plus one 90-day
//! aggregate per monitor per day. Memory is one snapshot (a few KB of JSON and
//! HTML) per published page plus 90 small counters per selected monitor.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::body::Bytes;
use chrono::{NaiveDate, Utc};
use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, QueryOrder, Statement, Value,
};
use temps_core::UtcDateTime;
use temps_entities::{projects, status_pages};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use super::render::render_public_page_html;
use super::types::{
    PublicComponentStatus, PublicDayStatus, PublicIncident, PublicIncidentSeverity,
    PublicIncidentStatus, PublicIncidentUpdate, PublicStatusComponent, PublicStatusPageResponse,
    PublicUptimeDay, PUBLIC_HISTORY_DAYS,
};

/// How often the background loop rebuilds every published snapshot.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// A check older than this no longer counts as the current state.
const CURRENT_STATUS_MAX_AGE_HOURS: i64 = 24;
/// Resolved incidents stay on the public page for this many days.
const RESOLVED_INCIDENT_RETENTION_DAYS: i64 = 14;
/// Most incidents shown per page (active ones first).
const MAX_INCIDENTS_PER_PAGE: i64 = 25;
/// Most public updates shown per incident.
const MAX_UPDATES_PER_INCIDENT: usize = 20;
/// Bound on bind parameters per `IN (...)` list; Postgres allows 65535.
const MAX_IDS_PER_QUERY: usize = 1000;

/// A published page, ready to serve.
#[derive(Debug)]
pub struct PublishedSnapshot {
    /// Internal bookkeeping only; never serialized.
    pub(crate) status_page_id: i32,
    pub(crate) project_id: i32,
    /// Custom hostnames (#1412) whose ownership is proven. A request tagged by
    /// the proxy with a hostname not in this set gets the 404, so removing a
    /// hostname takes effect here before the proxy's route table reloads.
    pub(crate) custom_hosts: HashSet<String>,
    pub json: Bytes,
    pub html: Bytes,
}

impl PublishedSnapshot {
    /// Whether this page may be served for a request tagged with
    /// `custom_host` (the proxy's `x-temps-status-host`), or for an untagged
    /// request to its slug URL.
    pub fn serves_host(&self, custom_host: Option<&str>) -> bool {
        match custom_host {
            None => true,
            Some(host) => self.custom_hosts.contains(host),
        }
    }
}

/// Which pages a rebuild covers.
#[derive(Debug, Clone, Copy)]
enum Scope {
    All,
    Page(i32),
    Project(i32),
}

impl Scope {
    fn covers(self, snapshot: &PublishedSnapshot) -> bool {
        match self {
            Scope::All => true,
            Scope::Page(id) => snapshot.status_page_id == id,
            Scope::Project(id) => snapshot.project_id == id,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DayCounts {
    pub operational: i64,
    pub degraded: i64,
    pub down: i64,
}

impl DayCounts {
    fn measured(self) -> i64 {
        self.operational + self.degraded + self.down
    }

    fn add(&mut self, other: DayCounts) {
        self.operational += other.operational;
        self.degraded += other.degraded;
        self.down += other.down;
    }
}

/// Completed days of history for one monitor, valid for `computed_on`.
struct MonitorHistory {
    computed_on: NaiveDate,
    past_days: HashMap<NaiveDate, DayCounts>,
}

#[derive(Default)]
struct WriterState {
    history: HashMap<i32, MonitorHistory>,
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct SelectionRow {
    pub status_page_id: i32,
    pub monitor_id: i32,
    pub display_name: String,
    pub environment_id: Option<i32>,
    pub is_active: bool,
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct IncidentRow {
    /// The page this row was selected for; an incident appears once per page.
    pub status_page_id: i32,
    pub id: i32,
    pub environment_id: Option<i32>,
    pub monitor_id: Option<i32>,
    pub severity: String,
    pub status: String,
    pub started_at: UtcDateTime,
    pub resolved_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PublicUpdateRow {
    pub incident_id: i32,
    pub status: String,
    pub message: String,
    pub created_at: UtcDateTime,
}

#[derive(Debug, FromQueryResult)]
struct DayRow {
    monitor_id: i32,
    bucket: UtcDateTime,
    operational: i64,
    degraded: i64,
    down: i64,
}

#[derive(Debug, FromQueryResult)]
struct LatestStatusRow {
    monitor_id: i32,
    status: String,
}

/// Everything a set of pages needs, fetched in batched queries.
#[derive(Default)]
pub(crate) struct RefreshData {
    /// Latest check status per monitor (internal vocabulary).
    pub latest_status: HashMap<i32, String>,
    /// Day counts per monitor, past days and today merged.
    pub days: HashMap<i32, HashMap<NaiveDate, DayCounts>>,
    /// Incidents with at least one public update that concern a page's
    /// selection, per status page.
    pub incidents: HashMap<i32, Vec<IncidentRow>>,
    /// Public updates per incident, newest first.
    pub updates: HashMap<i32, Vec<PublicUpdateRow>>,
}

pub struct StatusSnapshotCache {
    db: Arc<DatabaseConnection>,
    snapshots: ArcSwap<HashMap<String, Arc<PublishedSnapshot>>>,
    /// Serializes every writer. Mutations commit to the database *before*
    /// taking it, and every rebuild reads the database *after* taking it, so a
    /// background refresh can never re-insert a page that was unpublished
    /// while it ran.
    writer: Mutex<WriterState>,
}

impl StatusSnapshotCache {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            snapshots: ArcSwap::from_pointee(HashMap::new()),
            writer: Mutex::new(WriterState::default()),
        }
    }

    /// The published snapshot for `slug`, if any. Lock-free; no I/O.
    pub fn get(&self, slug: &str) -> Option<Arc<PublishedSnapshot>> {
        self.snapshots.load().get(slug).cloned()
    }

    /// Number of published pages currently served.
    pub fn len(&self) -> usize {
        self.snapshots.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Rebuild every published page.
    pub async fn refresh_all(&self) -> Result<usize, DbErr> {
        self.rebuild(Scope::All).await
    }

    /// Rebuild one page, removing it if it is no longer published.
    pub async fn refresh_page(&self, status_page_id: i32) -> Result<usize, DbErr> {
        self.rebuild(Scope::Page(status_page_id)).await
    }

    /// Rebuild every page of a project (e.g. after an incident update changed
    /// visibility).
    pub async fn refresh_project(&self, project_id: i32) -> Result<usize, DbErr> {
        self.rebuild(Scope::Project(project_id)).await
    }

    /// Stop serving a page. Infallible: no database access.
    pub async fn remove_page(&self, status_page_id: i32) {
        let _guard = self.writer.lock().await;
        self.swap_without(Scope::Page(status_page_id));
    }

    /// Stop serving every page of a deleted project.
    pub async fn remove_project(&self, project_id: i32) {
        let _guard = self.writer.lock().await;
        self.swap_without(Scope::Project(project_id));
    }

    /// Rebuild forever on [`REFRESH_INTERVAL`]. A failed refresh keeps the
    /// previous snapshots and is retried on the next tick.
    pub async fn run_refresh_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match self.refresh_all().await {
                Ok(count) => debug!(published_pages = count, "refreshed public status pages"),
                Err(e) => error!(
                    error = %e,
                    "failed to refresh public status page snapshots; serving the previous ones"
                ),
            }
        }
    }

    fn swap_without(&self, scope: Scope) {
        let current = self.snapshots.load();
        let next: HashMap<String, Arc<PublishedSnapshot>> = current
            .iter()
            .filter(|(_, snapshot)| !scope.covers(snapshot))
            .map(|(slug, snapshot)| (slug.clone(), snapshot.clone()))
            .collect();
        self.snapshots.store(Arc::new(next));
    }

    async fn rebuild(&self, scope: Scope) -> Result<usize, DbErr> {
        let mut state = self.writer.lock().await;
        let now = Utc::now();
        let db = self.db.as_ref();

        // A project being deleted is marked `is_deleted` before its rows are
        // removed; if that removal fails part-way, its pages must still go dark.
        let mut query = status_pages::Entity::find()
            .inner_join(projects::Entity)
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(status_pages::Column::IsPublished.eq(true));
        query = match scope {
            Scope::All => query,
            Scope::Page(id) => query.filter(status_pages::Column::Id.eq(id)),
            Scope::Project(id) => query.filter(status_pages::Column::ProjectId.eq(id)),
        };
        let pages = query.order_by_asc(status_pages::Column::Id).all(db).await?;

        let page_ids: Vec<i32> = pages.iter().map(|p| p.id).collect();
        let selections = load_selections(db, &page_ids).await?;
        let monitor_ids: Vec<i32> = selections
            .iter()
            .map(|s| s.monitor_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let data = load_refresh_data(db, &mut state, &monitor_ids, &page_ids, now).await?;
        let mut verified_hosts = super::domains::load_verified_hosts(db, &page_ids).await?;
        if matches!(scope, Scope::All) {
            let live: HashSet<i32> = monitor_ids.iter().copied().collect();
            state
                .history
                .retain(|monitor_id, _| live.contains(monitor_id));
        }

        let mut by_page: HashMap<i32, Vec<SelectionRow>> = HashMap::new();
        for selection in selections {
            by_page
                .entry(selection.status_page_id)
                .or_default()
                .push(selection);
        }

        let mut built = Vec::with_capacity(pages.len());
        for page in &pages {
            let page_selections = by_page.remove(&page.id).unwrap_or_default();
            let response = assemble_public_page(page, &page_selections, &data, now);
            match serde_json::to_vec(&response) {
                Ok(json) => built.push((
                    page.slug.clone(),
                    Arc::new(PublishedSnapshot {
                        status_page_id: page.id,
                        project_id: page.project_id,
                        custom_hosts: verified_hosts.remove(&page.id).unwrap_or_default(),
                        json: Bytes::from(json),
                        html: Bytes::from(render_public_page_html(&response)),
                    }),
                )),
                Err(e) => error!(
                    status_page_id = page.id,
                    error = %e,
                    "failed to serialize public status page snapshot; page will not be served"
                ),
            }
        }

        let count = built.len();
        let mut next: HashMap<String, Arc<PublishedSnapshot>> = match scope {
            Scope::All => HashMap::with_capacity(count),
            _ => self
                .snapshots
                .load()
                .iter()
                .filter(|(_, snapshot)| !scope.covers(snapshot))
                .map(|(slug, snapshot)| (slug.clone(), snapshot.clone()))
                .collect(),
        };
        next.extend(built);
        self.snapshots.store(Arc::new(next));

        if !matches!(scope, Scope::All) {
            info!(scope = ?scope, published = count, "rebuilt public status page snapshots");
        }
        Ok(count)
    }
}

fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn id_values(ids: &[i32]) -> Vec<Value> {
    ids.iter().map(|id| Value::from(*id)).collect()
}

async fn load_selections(
    db: &DatabaseConnection,
    page_ids: &[i32],
) -> Result<Vec<SelectionRow>, DbErr> {
    let mut rows = Vec::new();
    for chunk in page_ids.chunks(MAX_IDS_PER_QUERY) {
        // The join on `sm.project_id = sp.project_id` re-checks ownership at
        // read time, so a selection can never surface another project's
        // monitor even if one were ever written.
        let sql = format!(
            r#"
            SELECT spm.status_page_id, spm.monitor_id, spm.display_name,
                   sm.environment_id, sm.is_active
            FROM status_page_monitors spm
            JOIN status_pages sp ON sp.id = spm.status_page_id
            JOIN projects p ON p.id = sp.project_id AND NOT p.is_deleted
            JOIN status_monitors sm ON sm.id = spm.monitor_id AND sm.project_id = sp.project_id
            WHERE spm.status_page_id IN ({})
            ORDER BY spm.status_page_id, spm.position, spm.id
            "#,
            placeholders(1, chunk.len())
        );
        rows.extend(
            SelectionRow::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                id_values(chunk),
            ))
            .all(db)
            .await?,
        );
    }
    Ok(rows)
}

async fn load_day_counts(
    db: &DatabaseConnection,
    monitor_ids: &[i32],
    from: UtcDateTime,
    to: UtcDateTime,
) -> Result<HashMap<i32, HashMap<NaiveDate, DayCounts>>, DbErr> {
    let mut result: HashMap<i32, HashMap<NaiveDate, DayCounts>> = HashMap::new();
    for chunk in monitor_ids.chunks(MAX_IDS_PER_QUERY) {
        let n = chunk.len();
        let sql = format!(
            r#"
            SELECT monitor_id, bucket, operational, degraded, down
            FROM (
                SELECT monitor_id,
                       date_trunc('day', checked_at AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS bucket,
                       COUNT(*) FILTER (WHERE status = 'operational') AS operational,
                       COUNT(*) FILTER (WHERE status IN ('degraded', 'partial_outage')) AS degraded,
                       COUNT(*) FILTER (WHERE status IN ('down', 'major_outage')) AS down
                FROM status_checks
                WHERE monitor_id IN ({})
                  AND checked_at >= ${}
                  AND checked_at < ${}
                GROUP BY monitor_id, bucket
            ) sub
            "#,
            placeholders(1, n),
            n + 1,
            n + 2
        );
        let mut values = id_values(chunk);
        values.push(from.into());
        values.push(to.into());
        let rows = DayRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(db)
        .await?;
        for row in rows {
            result.entry(row.monitor_id).or_default().insert(
                row.bucket.date_naive(),
                DayCounts {
                    operational: row.operational,
                    degraded: row.degraded,
                    down: row.down,
                },
            );
        }
    }
    Ok(result)
}

async fn load_refresh_data(
    db: &DatabaseConnection,
    state: &mut WriterState,
    monitor_ids: &[i32],
    page_ids: &[i32],
    now: UtcDateTime,
) -> Result<RefreshData, DbErr> {
    let mut data = RefreshData::default();
    if monitor_ids.is_empty() && page_ids.is_empty() {
        return Ok(data);
    }

    let today = now.date_naive();
    let today_start = day_start(today);
    let history_start = day_start(today - chrono::Days::new(u64::from(PUBLIC_HISTORY_DAYS - 1)));

    // Completed days: recompute only for monitors whose cached history is
    // from an earlier day (or missing).
    let stale: Vec<i32> = monitor_ids
        .iter()
        .copied()
        .filter(|id| {
            state
                .history
                .get(id)
                .is_none_or(|history| history.computed_on != today)
        })
        .collect();
    if !stale.is_empty() {
        let mut past = load_day_counts(db, &stale, history_start, today_start).await?;
        for monitor_id in stale {
            state.history.insert(
                monitor_id,
                MonitorHistory {
                    computed_on: today,
                    past_days: past.remove(&monitor_id).unwrap_or_default(),
                },
            );
        }
    }

    // Today: a few hundred raw rows per monitor at most.
    let mut today_counts = if monitor_ids.is_empty() {
        HashMap::new()
    } else {
        load_day_counts(
            db,
            monitor_ids,
            today_start,
            now + chrono::Duration::seconds(1),
        )
        .await?
    };
    for monitor_id in monitor_ids {
        let mut days = state
            .history
            .get(monitor_id)
            .map(|history| history.past_days.clone())
            .unwrap_or_default();
        if let Some(today_days) = today_counts.remove(monitor_id) {
            for (day, counts) in today_days {
                days.entry(day).or_default().add(counts);
            }
        }
        data.days.insert(*monitor_id, days);
    }

    let since = now - chrono::Duration::hours(CURRENT_STATUS_MAX_AGE_HOURS);
    for chunk in monitor_ids.chunks(MAX_IDS_PER_QUERY) {
        let n = chunk.len();
        let sql = format!(
            r#"
            SELECT DISTINCT ON (monitor_id) monitor_id, status
            FROM status_checks
            WHERE monitor_id IN ({}) AND checked_at >= ${}
            ORDER BY monitor_id, checked_at DESC
            "#,
            placeholders(1, n),
            n + 1
        );
        let mut values = id_values(chunk);
        values.push(since.into());
        for row in LatestStatusRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(db)
        .await?
        {
            data.latest_status.insert(row.monitor_id, row.status);
        }
    }

    let cutoff = now - chrono::Duration::days(RESOLVED_INCIDENT_RETENTION_DAYS);
    let mut incident_ids = Vec::new();
    for chunk in page_ids.chunks(MAX_IDS_PER_QUERY) {
        let n = chunk.len();
        // Select per page, after applying that page's monitor selection, and
        // rank active incidents ahead of resolved history, so the per-page
        // limit can never push an active incident off the page.
        let sql = format!(
            r#"
            WITH pages AS (
                SELECT id AS status_page_id, project_id
                FROM status_pages
                WHERE id IN ({})
            ),
            sel AS (
                SELECT spm.status_page_id, spm.monitor_id, sm.environment_id
                FROM status_page_monitors spm
                JOIN pages p ON p.status_page_id = spm.status_page_id
                JOIN status_monitors sm
                  ON sm.id = spm.monitor_id AND sm.project_id = p.project_id
            ),
            candidates AS (
                SELECT p.status_page_id, i.id, i.project_id, i.environment_id,
                       i.monitor_id, i.severity, i.status, i.started_at, i.resolved_at
                FROM pages p
                JOIN status_incidents i ON i.project_id = p.project_id
                WHERE (i.status <> 'resolved' OR i.resolved_at >= ${} OR i.started_at >= ${})
                  AND (
                      (i.monitor_id IS NOT NULL AND EXISTS (
                          SELECT 1 FROM sel s
                          WHERE s.status_page_id = p.status_page_id
                            AND s.monitor_id = i.monitor_id))
                      OR (i.monitor_id IS NULL AND i.environment_id IS NULL)
                      OR (i.monitor_id IS NULL AND EXISTS (
                          SELECT 1 FROM sel s
                          WHERE s.status_page_id = p.status_page_id
                            AND s.environment_id = i.environment_id))
                  )
                  AND EXISTS (
                      SELECT 1 FROM status_incident_updates u
                      WHERE u.incident_id = i.id AND u.is_public AND NOT u.is_system
                  )
            )
            SELECT status_page_id, id, environment_id, monitor_id, severity,
                   status, started_at, resolved_at
            FROM (
                SELECT candidates.*,
                       ROW_NUMBER() OVER (
                           PARTITION BY status_page_id
                           ORDER BY (status = 'resolved'), started_at DESC, id DESC
                       ) AS rn
                FROM candidates
            ) ranked
            WHERE rn <= ${}
            ORDER BY status_page_id, started_at DESC, id DESC
            "#,
            placeholders(1, n),
            n + 1,
            n + 2,
            n + 3
        );
        let mut values = id_values(chunk);
        values.push(cutoff.into());
        values.push(cutoff.into());
        values.push(MAX_INCIDENTS_PER_PAGE.into());
        for row in IncidentRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(db)
        .await?
        {
            incident_ids.push(row.id);
            data.incidents
                .entry(row.status_page_id)
                .or_default()
                .push(row);
        }
    }
    incident_ids.sort_unstable();
    incident_ids.dedup();

    for chunk in incident_ids.chunks(MAX_IDS_PER_QUERY) {
        let n = chunk.len();
        // The per-incident limit is applied in SQL so a long-running incident
        // never makes a refresh read its whole history.
        let sql = format!(
            r#"
            SELECT incident_id, status, message, created_at
            FROM (
                SELECT incident_id, status, message, created_at, id,
                       ROW_NUMBER() OVER (
                           PARTITION BY incident_id ORDER BY created_at DESC, id DESC
                       ) AS rn
                FROM status_incident_updates
                WHERE is_public AND NOT is_system AND incident_id IN ({})
            ) ranked
            WHERE rn <= ${}
            ORDER BY incident_id, created_at DESC, id DESC
            "#,
            placeholders(1, n),
            n + 1
        );
        let mut values = id_values(chunk);
        values.push((MAX_UPDATES_PER_INCIDENT as i64).into());
        for row in PublicUpdateRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(db)
        .await?
        {
            data.updates.entry(row.incident_id).or_default().push(row);
        }
    }

    Ok(data)
}

fn day_start(day: NaiveDate) -> UtcDateTime {
    day.and_time(chrono::NaiveTime::MIN).and_utc()
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn uptime_percentage(counts: DayCounts) -> Option<f64> {
    let measured = counts.measured();
    (measured > 0).then(|| round2(counts.operational as f64 / measured as f64 * 100.0))
}

pub(crate) fn day_status(counts: DayCounts) -> PublicDayStatus {
    match uptime_percentage(counts) {
        None => PublicDayStatus::NoData,
        Some(pct) if counts.down > 0 && pct < 95.0 => PublicDayStatus::Outage,
        Some(_) if counts.down > 0 || counts.degraded > 0 => PublicDayStatus::Degraded,
        Some(_) => PublicDayStatus::Operational,
    }
}

/// Build the public view of one page from pre-fetched data. Pure: everything
/// it can publish is passed in, so tests can assert what reaches the output.
pub(crate) fn assemble_public_page(
    page: &status_pages::Model,
    selections: &[SelectionRow],
    data: &RefreshData,
    now: UtcDateTime,
) -> PublicStatusPageResponse {
    let today = now.date_naive();
    let first_day = today - chrono::Days::new(u64::from(PUBLIC_HISTORY_DAYS - 1));

    let empty_days = HashMap::new();
    let components: Vec<PublicStatusComponent> = selections
        .iter()
        .map(|selection| {
            let monitor_days = data.days.get(&selection.monitor_id).unwrap_or(&empty_days);
            let mut total = DayCounts::default();
            let days = first_day
                .iter_days()
                .take(PUBLIC_HISTORY_DAYS as usize)
                .map(|day| {
                    let counts = monitor_days.get(&day).copied().unwrap_or_default();
                    total.add(counts);
                    PublicUptimeDay {
                        date: day.format("%Y-%m-%d").to_string(),
                        status: day_status(counts),
                        uptime_percentage: uptime_percentage(counts),
                    }
                })
                .collect();
            let status = if selection.is_active {
                data.latest_status
                    .get(&selection.monitor_id)
                    .map(|s| PublicComponentStatus::from_check_status(s))
                    .unwrap_or(PublicComponentStatus::Unknown)
            } else {
                PublicComponentStatus::Unknown
            };
            PublicStatusComponent {
                name: selection.display_name.clone(),
                status,
                uptime_percentage: uptime_percentage(total),
                days,
            }
        })
        .collect();

    let incidents = public_incidents(page.id, selections, data);

    let mut status = components
        .iter()
        .map(|c| c.status)
        .filter(|s| *s != PublicComponentStatus::Unknown)
        .max()
        .unwrap_or(PublicComponentStatus::Unknown);
    for incident in incidents
        .iter()
        .filter(|i| i.status != PublicIncidentStatus::Resolved)
    {
        let floor = match incident.severity {
            PublicIncidentSeverity::Critical => PublicComponentStatus::Outage,
            PublicIncidentSeverity::Major | PublicIncidentSeverity::Minor => {
                PublicComponentStatus::Degraded
            }
        };
        status = status.max(floor);
    }

    PublicStatusPageResponse {
        title: page.title.clone(),
        description: page.description.clone(),
        status,
        components,
        incidents,
        history_days: PUBLIC_HISTORY_DAYS,
        generated_at: now,
    }
}

/// Incidents selected for `status_page_id` that concern this page's selection
/// and have at least one public update. The query already applies these
/// rules; they are re-applied here so the output never depends on it alone.
///
/// - linked to a monitor: shown only if that monitor is selected;
/// - linked to an environment only: shown only if a selected monitor watches
///   that environment;
/// - project-wide: shown on every page of the project.
fn public_incidents(
    status_page_id: i32,
    selections: &[SelectionRow],
    data: &RefreshData,
) -> Vec<PublicIncident> {
    let Some(incidents) = data.incidents.get(&status_page_id) else {
        return Vec::new();
    };
    let names_by_monitor: BTreeMap<i32, &str> = selections
        .iter()
        .map(|s| (s.monitor_id, s.display_name.as_str()))
        .collect();

    incidents
        .iter()
        .filter_map(|incident| {
            let affected: Vec<String> = match (incident.monitor_id, incident.environment_id) {
                (Some(monitor_id), _) => {
                    vec![names_by_monitor.get(&monitor_id)?.to_string()]
                }
                (None, Some(environment_id)) => {
                    let names: Vec<String> = selections
                        .iter()
                        .filter(|s| s.environment_id == Some(environment_id))
                        .map(|s| s.display_name.clone())
                        .collect();
                    if names.is_empty() {
                        return None;
                    }
                    names
                }
                (None, None) => Vec::new(),
            };
            let updates: Vec<PublicIncidentUpdate> = data
                .updates
                .get(&incident.id)?
                .iter()
                .map(|u| PublicIncidentUpdate {
                    status: PublicIncidentStatus::from_internal(&u.status),
                    message: u.message.clone(),
                    created_at: u.created_at,
                })
                .collect();
            let latest = updates.first()?;
            let resolved = incident.status == "resolved";
            Some(PublicIncident {
                severity: PublicIncidentSeverity::from_internal(&incident.severity),
                status: if resolved {
                    PublicIncidentStatus::Resolved
                } else {
                    latest.status
                },
                affected_components: affected,
                started_at: incident.started_at,
                resolved_at: if resolved { incident.resolved_at } else { None },
                updates,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Fixed clock: whole seconds, so no timestamp can accidentally contain
    /// the id digits the leak test searches for.
    fn now() -> UtcDateTime {
        Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0)
            .single()
            .expect("valid fixed timestamp")
    }

    fn page() -> status_pages::Model {
        let now = now();
        status_pages::Model {
            id: 7,
            project_id: 3,
            slug: "0123456789abcdef0123456789abcdef".to_string(),
            title: "Public title".to_string(),
            description: None,
            is_published: true,
            published_at: Some(now),
            created_at: now,
            updated_at: now,
        }
    }

    fn selection(monitor_id: i32, name: &str, environment_id: Option<i32>) -> SelectionRow {
        SelectionRow {
            status_page_id: 7,
            monitor_id,
            display_name: name.to_string(),
            environment_id,
            is_active: true,
        }
    }

    fn incident(id: i32, monitor_id: Option<i32>, environment_id: Option<i32>) -> IncidentRow {
        IncidentRow {
            status_page_id: 7,
            id,
            environment_id,
            monitor_id,
            severity: "major".to_string(),
            status: "investigating".to_string(),
            started_at: now(),
            resolved_at: None,
        }
    }

    fn update(incident_id: i32, message: &str) -> PublicUpdateRow {
        PublicUpdateRow {
            incident_id,
            status: "identified".to_string(),
            message: message.to_string(),
            created_at: now(),
        }
    }

    #[test]
    fn day_status_thresholds() {
        let none = DayCounts::default();
        assert_eq!(day_status(none), PublicDayStatus::NoData);
        let clean = DayCounts {
            operational: 100,
            ..Default::default()
        };
        assert_eq!(day_status(clean), PublicDayStatus::Operational);
        let blip = DayCounts {
            operational: 99,
            down: 1,
            ..Default::default()
        };
        assert_eq!(day_status(blip), PublicDayStatus::Degraded);
        let outage = DayCounts {
            operational: 50,
            down: 50,
            ..Default::default()
        };
        assert_eq!(day_status(outage), PublicDayStatus::Outage);
        let slow = DayCounts {
            degraded: 10,
            ..Default::default()
        };
        assert_eq!(day_status(slow), PublicDayStatus::Degraded);
    }

    #[test]
    fn components_cover_exactly_the_history_window_with_display_names() {
        let now = now();
        let mut data = RefreshData::default();
        data.latest_status.insert(1, "major_outage".to_string());
        data.days.insert(
            1,
            HashMap::from([(
                now.date_naive(),
                DayCounts {
                    operational: 3,
                    down: 1,
                    ..Default::default()
                },
            )]),
        );
        let out = assemble_public_page(&page(), &[selection(1, "API", None)], &data, now);
        assert_eq!(out.components.len(), 1);
        let component = &out.components[0];
        assert_eq!(component.name, "API");
        assert_eq!(component.status, PublicComponentStatus::Outage);
        assert_eq!(component.days.len(), PUBLIC_HISTORY_DAYS as usize);
        assert_eq!(
            component.days.last().map(|d| d.date.clone()),
            Some(now.date_naive().format("%Y-%m-%d").to_string())
        );
        assert_eq!(component.uptime_percentage, Some(75.0));
        assert_eq!(out.status, PublicComponentStatus::Outage);
    }

    #[test]
    fn paused_or_silent_monitors_are_unknown_and_do_not_drive_overall_status() {
        let mut paused = selection(1, "Paused", None);
        paused.is_active = false;
        let mut data = RefreshData::default();
        data.latest_status.insert(1, "down".to_string());
        data.latest_status.insert(2, "operational".to_string());
        let out = assemble_public_page(
            &page(),
            &[
                paused,
                selection(2, "Web", None),
                selection(3, "Silent", None),
            ],
            &data,
            now(),
        );
        assert_eq!(out.components[0].status, PublicComponentStatus::Unknown);
        assert_eq!(out.components[2].status, PublicComponentStatus::Unknown);
        assert_eq!(out.status, PublicComponentStatus::Operational);
    }

    #[test]
    fn unknown_internal_status_never_surfaces_verbatim() {
        let mut data = RefreshData::default();
        data.latest_status
            .insert(1, "connection refused to 10.0.0.5".to_string());
        let out = assemble_public_page(&page(), &[selection(1, "API", None)], &data, now());
        assert_eq!(out.components[0].status, PublicComponentStatus::Unknown);
        let json = serde_json::to_string(&out).expect("serialize");
        assert!(!json.contains("10.0.0.5"));
    }

    #[test]
    fn incidents_on_unselected_monitors_or_environments_are_excluded() {
        let mut data = RefreshData::default();
        data.incidents.insert(
            7,
            vec![
                incident(10, Some(1), Some(100)), // selected monitor
                incident(11, Some(2), Some(100)), // unselected monitor
                incident(12, None, Some(100)),    // environment of a selected monitor
                incident(13, None, Some(200)),    // environment with no selected monitor
                incident(14, None, None),         // project-wide
                incident(15, Some(1), None),      // selected monitor, but no public update
            ],
        );
        for id in [10, 11, 12, 13, 14] {
            data.updates
                .insert(id, vec![update(id, &format!("public message {id}"))]);
        }
        let out = assemble_public_page(&page(), &[selection(1, "API", Some(100))], &data, now());
        let messages: Vec<&str> = out
            .incidents
            .iter()
            .map(|i| i.updates[0].message.as_str())
            .collect();
        assert_eq!(
            messages,
            vec![
                "public message 10",
                "public message 12",
                "public message 14"
            ]
        );
        assert_eq!(out.incidents[0].affected_components, vec!["API"]);
        assert_eq!(out.incidents[1].affected_components, vec!["API"]);
        assert!(out.incidents[2].affected_components.is_empty());
        // An active major incident lifts an otherwise unknown page to degraded.
        assert_eq!(out.status, PublicComponentStatus::Degraded);
    }

    #[test]
    fn resolved_incident_reports_resolution_time_and_status() {
        let mut data = RefreshData::default();
        let mut resolved = incident(10, None, None);
        resolved.status = "resolved".to_string();
        resolved.resolved_at = Some(now());
        data.incidents.insert(7, vec![resolved]);
        data.updates.insert(10, vec![update(10, "fixed")]);
        let out = assemble_public_page(&page(), &[], &data, now());
        assert_eq!(out.incidents[0].status, PublicIncidentStatus::Resolved);
        assert!(out.incidents[0].resolved_at.is_some());
        assert_eq!(out.status, PublicComponentStatus::Unknown);
    }

    #[test]
    fn public_json_contains_no_internal_identifiers() {
        let mut data = RefreshData::default();
        data.latest_status.insert(4242, "operational".to_string());
        data.incidents
            .insert(7, vec![incident(9191, Some(4242), Some(5151))]);
        data.updates.insert(9191, vec![update(9191, "all good")]);
        let out = assemble_public_page(
            &page(),
            &[selection(4242, "Checkout", Some(5151))],
            &data,
            now(),
        );
        let json = serde_json::to_string(&out).expect("serialize");
        for forbidden in [
            "4242", "9191", "5151", "\"id\"", "project", "monitor", "slug",
        ] {
            assert!(
                !json.contains(forbidden),
                "public JSON must not contain {forbidden:?}: {json}"
            );
        }
    }
}
