// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::traits::Analytics;
use crate::types::responses::{
    self, DropOffPoint, EnrichVisitorResponse, EventCount, PageFlowEntry, PageFlowResponse,
    PageTransition, SessionDetails, SessionEventsResponse, SessionLogsResponse, VisitorDetails,
    VisitorFacetValue, VisitorFacets, VisitorSessionsResponse, VisitorsResponse,
};
use crate::types::{AnalyticsError, Page};
use async_trait::async_trait;
use sea_orm::{
    ColumnTrait, DatabaseBackend, DatabaseConnection, EntityTrait, FromQueryResult, PaginatorTrait,
    QueryFilter, QueryOrder, Statement,
};
use std::sync::Arc;
use temps_core::{CookieCrypto, UtcDateTime};
use temps_entities::{events, request_sessions, visitor};

pub struct AnalyticsService {
    db: Arc<DatabaseConnection>,
    cookie_crypto: Arc<CookieCrypto>,
}
impl AnalyticsService {
    pub fn new(db: Arc<DatabaseConnection>, cookie_crypto: Arc<CookieCrypto>) -> Self {
        AnalyticsService { db, cookie_crypto }
    }

    /// Build the visitor-row WHERE clause + parameter list shared by the
    /// facet queries. Returns the predicates joined with ` AND `, the bound
    /// values, the next `$N` parameter index, and whether an
    /// `ip_geolocations` join is required.
    fn build_visitor_segment_predicates(
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        include_crawlers: Option<bool>,
        has_activity_only: Option<bool>,
        segment: &crate::types::requests::VisitorSegmentFilters,
    ) -> (String, Vec<sea_orm::Value>, usize, bool) {
        let mut where_conditions: Vec<String> = vec!["v.project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let mut param_index = 2;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("v.environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }
        if include_crawlers == Some(false) {
            where_conditions.push("v.is_crawler = false".to_string());
        }
        if has_activity_only == Some(true) {
            where_conditions.push("v.has_activity = true".to_string());
        }

        where_conditions.push(format!("v.last_seen >= ${}", param_index));
        values.push(start_date.into());
        param_index += 1;
        where_conditions.push(format!("v.last_seen <= ${}", param_index));
        values.push(end_date.into());
        param_index += 1;

        let needs_geo_join = segment.filter_country.is_some()
            || segment.filter_region.is_some()
            || segment.filter_city.is_some();

        if let Some(country) = &segment.filter_country {
            where_conditions.push(format!("ig.country = ${}", param_index));
            values.push(country.clone().into());
            param_index += 1;
        }
        if let Some(region) = &segment.filter_region {
            where_conditions.push(format!("ig.region = ${}", param_index));
            values.push(region.clone().into());
            param_index += 1;
        }
        if let Some(city) = &segment.filter_city {
            where_conditions.push(format!("ig.city = ${}", param_index));
            values.push(city.clone().into());
            param_index += 1;
        }

        if let Some(channel) = &segment.filter_channel {
            where_conditions.push(format!("v.first_channel = ${}", param_index));
            values.push(channel.clone().into());
            param_index += 1;
        }
        if let Some(referrer) = &segment.filter_referrer {
            if referrer == "Direct" {
                where_conditions.push("v.first_referrer_hostname IS NULL".to_string());
            } else {
                where_conditions.push(format!("v.first_referrer_hostname = ${}", param_index));
                values.push(referrer.clone().into());
                param_index += 1;
            }
        }

        (
            where_conditions.join(" AND "),
            values,
            param_index,
            needs_geo_join,
        )
    }

    /// Aggregate a visitor-row dimension (country/region/city/channel/referrer).
    ///
    /// `code_expr` is an optional second SELECT expression used to carry an
    /// auxiliary code alongside the value (country_code for country flags).
    async fn facet_visitor_dimension(
        &self,
        value_expr: &str,
        code_expr: Option<&str>,
        geo_mode: FacetGeoMode,
        scope: &FacetScope<'_>,
        segment: &crate::types::requests::VisitorSegmentFilters,
    ) -> Result<Vec<VisitorFacetValue>, AnalyticsError> {
        let (where_clause, mut values, next_index, needs_geo_join) =
            Self::build_visitor_segment_predicates(
                scope.start_date,
                scope.end_date,
                scope.project_id,
                scope.environment_id,
                scope.include_crawlers,
                scope.has_activity_only,
                segment,
            );

        let join_geo = matches!(geo_mode, FacetGeoMode::Always) || needs_geo_join;
        let geo_join = if join_geo {
            "LEFT JOIN ip_geolocations ig ON v.ip_address_id = ig.id"
        } else {
            ""
        };

        let code_select = code_expr
            .map(|c| format!(", {} AS code", c))
            .unwrap_or_default();
        let code_group = code_expr.map(|c| format!(", {}", c)).unwrap_or_default();

        let sql = format!(
            r#"
            SELECT {value} AS value{code_select}, COUNT(DISTINCT v.id) AS count
            FROM visitor v
            {geo_join}
            WHERE {where_clause}
              AND {value} IS NOT NULL
              AND {value} <> ''
            GROUP BY {value}{code_group}
            ORDER BY count DESC, value ASC
            LIMIT ${limit_idx}
            "#,
            value = value_expr,
            code_select = code_select,
            geo_join = geo_join,
            where_clause = where_clause,
            code_group = code_group,
            limit_idx = next_index,
        );
        values.push((scope.limit as i64).into());

        #[derive(FromQueryResult)]
        struct Row {
            value: Option<String>,
            code: Option<String>,
            count: i64,
        }

        // The `code` column may not exist in the SELECT; sea_orm's
        // FromQueryResult will tolerate a missing column when the field is
        // `Option<T>`, but we still need a row type that compiles. So we use
        // a separate query type when there's no code.
        if code_expr.is_some() {
            let rows = Row::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .all(self.db.as_ref())
            .await?;
            Ok(rows
                .into_iter()
                .filter_map(|r| {
                    r.value.map(|v| VisitorFacetValue {
                        value: v,
                        code: r.code,
                        count: r.count,
                    })
                })
                .collect())
        } else {
            #[derive(FromQueryResult)]
            struct RowNoCode {
                value: Option<String>,
                count: i64,
            }
            let rows = RowNoCode::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .all(self.db.as_ref())
            .await?;
            Ok(rows
                .into_iter()
                .filter_map(|r| {
                    r.value.map(|v| VisitorFacetValue {
                        value: v,
                        code: None,
                        count: r.count,
                    })
                })
                .collect())
        }
    }
}

/// Controls whether `facet_visitor_dimension` joins `ip_geolocations`
/// unconditionally. Used so country/region/city facets join even when no
/// geo segment is set.
#[derive(Clone, Copy)]
enum FacetGeoMode {
    Always,
    IfFiltered,
}

/// Shared scope of a single `get_visitor_facets` call. Threaded through every
/// per-dimension query so the helpers stay under the clippy arg-count limit.
struct FacetScope<'a> {
    start_date: UtcDateTime,
    end_date: UtcDateTime,
    project_id: i32,
    environment_id: Option<i32>,
    include_crawlers: Option<bool>,
    has_activity_only: Option<bool>,
    limit: i32,
    _marker: std::marker::PhantomData<&'a ()>,
}

/// Largest serialized `custom_data` a project-scoped caller (a deployment
/// token) may leave on a visitor row.
pub const MAX_SCOPED_ENRICHMENT_BYTES: usize = 8 * 1024;
/// Most top-level keys a project-scoped caller may leave on a visitor row.
pub const MAX_SCOPED_ENRICHMENT_KEYS: usize = 32;

/// Absolute ceiling on a visitor's `custom_data` for callers that are not
/// project-scoped (session, API key, CLI). Now that enrichment merges, repeated
/// writes accumulate, so even trusted callers get a bound.
pub const MAX_ENRICHMENT_BYTES: usize = 64 * 1024;
/// See [`MAX_ENRICHMENT_BYTES`].
pub const MAX_ENRICHMENT_KEYS: usize = 128;

/// How far before the requested window `get_page_path_visitors` and
/// `get_page_path_detail` read a session's page views in full (to number them,
/// `session_page_number`, and for the detail's entry and exit pages).
///
/// A session is a sliding 30-minute inactivity window, so one continuously
/// active client can keep a single session for days. Reading every event such
/// a session ever had made the request cost grow with the session's whole
/// history; this bound caps it. A session that already had page views more
/// than this long before the window is numbered from the first page view
/// inside the bound, so its page number is lower than the true one.
/// `is_entry` and `is_bounce` stay exact, and so do the detail's entry, exit
/// and bounce rates: lookups bounded by the session's own lifetime cover the
/// page views outside the bound (see [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`],
/// including its limit for older keyed sessions).
const PAGE_VISITORS_SESSION_LOOKBACK_HOURS: i64 = 24;

/// How far past the requested window `get_page_path_visitors` reads a
/// session's later events. It must be at least the 30-minute page_leave
/// horizon of the time-on-page calculation; any later page view only needs
/// an existence check, since it puts time on page at the 1800s clamp.
const PAGE_VISITORS_SESSION_LOOKAHEAD_MINUTES: i64 = 30;

/// How far outside a visitor's `first_seen` or a session's
/// `[started_at, last_accessed_at]` its rows may be timestamped. Events are
/// stamped with the server clock at ingest after the visitor row exists, and a
/// request log only carries a session id once the proxy has upserted that
/// session. The proxy re-issues the session cookie (`Max-Age` = the session
/// timeout, 30 minutes by default) only on the tracked document responses that
/// also move `last_accessed_at`, so a browser stops sending a session id within
/// that timeout of the session's last recorded access. Keyed (SDK) sessions are
/// not proxied; event ingest moves their `last_accessed_at` forward at least
/// every few minutes of activity (`SESSION_LAST_ACCESS_REFRESH_SECONDS` in
/// `temps-analytics-events`). The gaps are therefore the session timeout, that
/// refresh interval, batch-writer delay and clock skew between proxy nodes
/// and the control plane; a day is far beyond them. Used as a time bound so
/// TimescaleDB skips chunks (including compressed ones, which cannot use the
/// visitor or session index) that cannot hold the rows.
///
/// Keyed sessions ingested before that refresh existed kept the time of their
/// first event in `last_accessed_at`. For one of those that stayed active for
/// more than a day, page views later than a day after its first event are
/// not seen by the lookups past a window: its exit and bounce come from the
/// events inside the read bound, as if the session ended there, and its last
/// page view in the bound gets the fallback time on page (the 1s floor in the
/// visitors list, 30s in the detail average). It goes away as event
/// retention removes those sessions' events.
const ANALYTICS_ROW_CLOCK_SLACK_HOURS: i32 = 24;

/// `ts` minus [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`], saturated at the minimum
/// timestamp.
fn earliest_row_time(ts: UtcDateTime) -> UtcDateTime {
    ts.checked_sub_signed(chrono::Duration::hours(i64::from(
        ANALYTICS_ROW_CLOCK_SLACK_HOURS,
    )))
    .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
}

/// `get_page_path_visitors`' query: one page of page views with their
/// session-flow details.
///
/// The cost is bounded by the page, not by the sessions behind it:
///   1. target_views picks the requested page of page views first
///      (an index range scan on project/type/path/timestamp).
///   2. Only the sessions of those <= per_page views are read, each
///      through its own session_id index lookup, bounded to
///      [$8, $9], and every per-view value comes from window
///      functions over that session's page views: O(n log n) in the
///      session's bounded page views. Self-joins of the session's
///      events against themselves were O(n^2) and timed out on a
///      session holding thousands of page views.
///   3. request_sessions is resolved once per session, not per event.
///
/// time_on_page keeps the 3-priority COALESCE, clamped to [1, 1800]s:
///   Priority 1: next page_leave for the same session+page within
///     30 min (an indexed lookup per view).
///   Priority 2: next page_view in the same session (any page). A
///     next page view past $9 is at least 30 min away and so lands on
///     the 1800s clamp: only its existence is checked.
///   Priority 3: the session's own last_accessed_at, which covers the
///     exit page (no next event), then a 1s floor.
/// is_entry / is_exit / is_bounce also account for the session's page
/// views before $8 and after $9 (existence checks bounded by the session's
/// own lifetime, see [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`], including its limit
/// for older keyed sessions), so they are
/// exact; session_page_number counts page views from $8 (see
/// [`PAGE_VISITORS_SESSION_LOOKBACK_HOURS`]).
///
/// Parameters: `$1` project_id, `$2` page_path, `$3`/`$4` window start/end,
/// `$5` environment_id (NULL for all), `$6`/`$7` LIMIT/OFFSET, `$8`/`$9` the
/// session-event bounds from [`page_path_visitors_context_bounds`], `$10`
/// [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`].
const PAGE_PATH_VISITORS_SQL: &str = r#"
            WITH target_views AS MATERIALIZED (
                SELECT
                    e.id AS event_id,
                    e.session_id,
                    e.timestamp AS viewed_at,
                    e.visitor_id,
                    e.referrer,
                    e.browser,
                    e.operating_system,
                    e.device_type,
                    e.ip_geolocation_id
                FROM events e
                WHERE e.project_id = $1
                  AND e.event_type = 'page_view'
                  AND e.page_path = $2
                  AND e.timestamp >= $3
                  AND e.timestamp <= $4
                  AND e.is_crawler = false
                  -- A view without a session has no session flow to report.
                  AND e.session_id IS NOT NULL
                  AND ($5::int IS NULL OR e.environment_id = $5)
                -- event_id breaks timestamp ties so pages never overlap.
                ORDER BY e.timestamp DESC, e.id DESC
                LIMIT $6 OFFSET $7
            ),
            target_sessions AS MATERIALIZED (
                SELECT session_id, array_agg(event_id) AS target_event_ids
                FROM target_views
                GROUP BY session_id
            ),
            -- Position and next page view of each target view within its
            -- session. LATERAL keeps this a per-session index lookup whatever
            -- the row estimates, and filtering each session's rows to its own
            -- target ids keeps the later joins at <= per_page rows. The
            -- events.is_entry / is_exit / is_bounce / session_page_number
            -- columns are never populated by the ingest path, so they are
            -- derived from event order here instead.
            session_page_views AS MATERIALIZED (
                SELECT w.event_id, w.pv_number, w.pv_total, w.next_page_view_ts
                FROM target_sessions ts
                CROSS JOIN LATERAL (
                    SELECT
                        e.id AS event_id,
                        ROW_NUMBER() OVER (ORDER BY e.timestamp ASC, e.id ASC) AS pv_number,
                        COUNT(*) OVER () AS pv_total,
                        -- Earliest page view strictly after this one: the
                        -- running MIN over the rows already seen in
                        -- descending order, excluding same-timestamp peers.
                        -- The frame head stays at the partition start, so
                        -- PostgreSQL accumulates it in a single pass.
                        MIN(e.timestamp) OVER (
                            ORDER BY e.timestamp DESC
                            RANGE BETWEEN UNBOUNDED PRECEDING AND '1 microsecond' PRECEDING
                        ) AS next_page_view_ts
                    FROM events e
                    WHERE e.session_id = ts.session_id
                      AND e.project_id = $1
                      AND e.event_type = 'page_view'
                      AND e.timestamp >= $8
                      AND e.timestamp <= $9
                ) w
                WHERE w.event_id = ANY(ts.target_event_ids)
            ),
            session_lifetimes AS MATERIALIZED (
                SELECT
                    ts.session_id,
                    -- Tolerate both bare-UUID and legacy v2|uuid|ts formats in
                    -- events.session_id (see VISITOR_SESSIONS_SQL): two
                    -- unique-index lookups per session.
                    COALESCE(rs_exact.started_at, rs_v2.started_at) AS session_started_at,
                    COALESCE(rs_exact.last_accessed_at, rs_v2.last_accessed_at)
                        AS session_last_accessed_at
                FROM target_sessions ts
                LEFT JOIN request_sessions rs_exact ON rs_exact.session_id = ts.session_id
                LEFT JOIN request_sessions rs_v2
                    ON ts.session_id LIKE 'v2|%'
                    AND rs_v2.session_id = split_part(ts.session_id, '|', 2)
            ),
            -- Whether the session has page views outside [$8, $9]. Each
            -- probe is bounded by the session's own lifetime (widened by the
            -- clock slack), so only the chunks the session spans are read,
            -- never everything up to now or back to the project's start. A
            -- session without a request_sessions row (yet) falls back to
            -- the slack around the bounds.
            session_meta AS MATERIALIZED (
                SELECT
                    sl.session_id,
                    sl.session_last_accessed_at,
                    EXISTS (
                        SELECT 1
                        FROM events earlier
                        WHERE earlier.session_id = sl.session_id
                          AND earlier.project_id = $1
                          AND earlier.event_type = 'page_view'
                          AND earlier.timestamp < $8
                          AND earlier.timestamp >= COALESCE(sl.session_started_at, $8)
                              - make_interval(hours => $10)
                    ) AS has_page_view_before_bound,
                    EXISTS (
                        SELECT 1
                        FROM events later
                        WHERE later.session_id = sl.session_id
                          AND later.project_id = $1
                          AND later.event_type = 'page_view'
                          AND later.timestamp > $9
                          AND later.timestamp <= COALESCE(sl.session_last_accessed_at, $9)
                              + make_interval(hours => $10)
                    ) AS has_page_view_after_bound
                FROM session_lifetimes sl
            )
            SELECT
                tv.visitor_id,
                COALESCE(v.visitor_id, '') AS visitor_uuid,
                tv.session_id,
                tv.viewed_at,
                CASE
                    -- The next page view lies past $9, so at least 30 min
                    -- after this one: exactly the upper clamp.
                    WHEN nl.next_page_leave_ts IS NULL
                     AND spv.next_page_view_ts IS NULL
                     AND sm.has_page_view_after_bound
                    THEN 1800
                    -- Priority 3 falls back to the session's own last-activity
                    -- timestamp (not a fixed guess) so the exit page -- which by
                    -- definition has no next event to diff against -- still gets
                    -- a real duration instead of NULL. Clamped to [1, 1800] so
                    -- bounces still render a nonzero value and stale/anomalous
                    -- gaps don't inflate it.
                    ELSE LEAST(1800, GREATEST(1, EXTRACT(EPOCH FROM (
                        COALESCE(
                            nl.next_page_leave_ts,
                            spv.next_page_view_ts,
                            sm.session_last_accessed_at,
                            tv.viewed_at + INTERVAL '1 second'
                        ) - tv.viewed_at
                    ))::int))
                END AS time_on_page,
                (spv.pv_number = 1 AND NOT sm.has_page_view_before_bound) AS is_entry,
                (spv.pv_number = spv.pv_total AND NOT sm.has_page_view_after_bound) AS is_exit,
                (
                    spv.pv_total <= 1
                    AND NOT sm.has_page_view_before_bound
                    AND NOT sm.has_page_view_after_bound
                ) AS is_bounce,
                spv.pv_number::int AS session_page_number,
                tv.referrer,
                tv.browser,
                tv.operating_system,
                tv.device_type,
                g.city,
                g.country,
                g.country_code
            FROM target_views tv
            JOIN session_page_views spv ON spv.event_id = tv.event_id
            JOIN session_meta sm ON sm.session_id = tv.session_id
            -- Priority 1: next page_leave for the same session+page within
            -- 30 min.
            LEFT JOIN LATERAL (
                SELECT MIN(pl.timestamp) AS next_page_leave_ts
                FROM events pl
                WHERE pl.session_id = tv.session_id
                  AND pl.project_id = $1
                  AND pl.event_type = 'page_leave'
                  AND pl.page_path = $2
                  AND pl.timestamp > tv.viewed_at
                  AND pl.timestamp <= tv.viewed_at + INTERVAL '30 minutes'
                  AND pl.timestamp >= $3
                  AND pl.timestamp <= $9
            ) nl ON true
            LEFT JOIN visitor v ON v.id = tv.visitor_id
            LEFT JOIN ip_geolocations g ON g.id = tv.ip_geolocation_id
            ORDER BY tv.viewed_at DESC, tv.event_id DESC
            "#;

/// `get_visitor_sessions_by_id`' query. Parameters: `$1` visitor id, `$2`
/// limit, `$3` the visitor's project, `$4` [`earliest_row_time`] of its
/// `first_seen`, `$5` [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`].
///
/// One pass over the visitor's events, aggregated per raw
/// events.session_id; request_sessions is then resolved once per
/// distinct raw id (not once per event), and the raw groups that map to
/// the same request_sessions row are combined. The scan is bounded by
/// project (compressed chunks are segmented by project_id) and by the
/// visitor's first_seen (see ANALYTICS_ROW_CLOCK_SLACK_HOURS).
///
/// Raw ids come in two formats: the bare UUID (new events, after the
/// session-cookie normalisation fix) and the legacy v2|uuid|ts format
/// stored by older versions of the analytics ingest path.
/// `split_part(raw_session_id, '|', 2)` extracts the UUID segment from
/// `v2|<uuid>|<ts>`; the LIKE guard avoids a false match on empty
/// strings when the id has no '|' delimiters. An exact match wins.
///
/// Proxy request count for each listed session: proxy_logs is the live
/// request-log table (request_logs is defined in the schema but never
/// written). Counted after LIMIT, so at most $2 sessions. Only browser
/// documents attach a session to a request, and each also moves the
/// session's last_accessed_at, so a session's request logs lie within
/// `[started_at, last_accessed_at]` up to the batch-writer delay; the
/// bound (widened by ANALYTICS_ROW_CLOCK_SLACK_HOURS) lets TimescaleDB
/// skip every other chunk. A NULL session_id matches nothing and
/// yields 0. The project_id guard is defense-in-depth (session ids are
/// globally unique PKs already); the IS NULL arm keeps proxy rows
/// written before project attribution.
const VISITOR_SESSIONS_SQL: &str = r#"            WITH raw_sessions AS (
                SELECT
                    e.session_id AS raw_session_id,
                    MIN(e.timestamp) AS started_at,
                    MAX(e.timestamp) AS ended_at,
                    COUNT(*) FILTER (WHERE e.event_type = 'page_view') AS page_views,
                    COUNT(*) AS events_count,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp ASC))[1] AS entry_path,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp DESC))[1] AS exit_path,
                    MIN(e.referrer) AS referrer,
                    COUNT(*) FILTER (
                        WHERE e.event_type NOT IN ('page_view', 'page_leave')
                          AND e.event_type NOT LIKE '%\_viewed' ESCAPE '\'
                    ) AS interactions
                FROM events e
                WHERE e.visitor_id = $1
                  AND e.project_id = $3
                  AND e.session_id IS NOT NULL
                  AND e.timestamp >= $4
                GROUP BY e.session_id
            ),
            resolved AS (
                SELECT
                    raw.*,
                    COALESCE(rs_exact.id, rs_v2.id) AS session_id,
                    COALESCE(rs_exact.started_at, rs_v2.started_at) AS rs_started_at,
                    COALESCE(rs_exact.last_accessed_at, rs_v2.last_accessed_at) AS rs_last_accessed_at
                FROM raw_sessions raw
                LEFT JOIN LATERAL (
                    SELECT rs.id, rs.started_at, rs.last_accessed_at
                    FROM request_sessions rs
                    WHERE rs.session_id = raw.raw_session_id
                    LIMIT 1
                ) rs_exact ON true
                LEFT JOIN LATERAL (
                    SELECT rs.id, rs.started_at, rs.last_accessed_at
                    FROM request_sessions rs
                    WHERE rs_exact.id IS NULL
                      AND raw.raw_session_id LIKE 'v2|%'
                      AND rs.session_id = split_part(raw.raw_session_id, '|', 2)
                    LIMIT 1
                ) rs_v2 ON true
            ),
            session_stats AS (
                SELECT
                    session_id,
                    MIN(started_at) AS started_at,
                    MAX(ended_at) AS ended_at,
                    EXTRACT(EPOCH FROM (MAX(ended_at) - MIN(started_at))) AS duration_seconds,
                    SUM(page_views)::bigint AS page_views,
                    SUM(events_count)::bigint AS events_count,
                    (ARRAY_AGG(entry_path ORDER BY started_at ASC))[1] AS entry_path,
                    (ARRAY_AGG(exit_path ORDER BY ended_at DESC))[1] AS exit_path,
                    MIN(referrer) AS referrer,
                    SUM(page_views) <= 1 AS is_bounced,
                    (
                        EXTRACT(EPOCH FROM (MAX(ended_at) - MIN(started_at))) >= 10
                        OR SUM(interactions) > 0
                    ) AS is_engaged,
                    MIN(rs_started_at) AS rs_started_at,
                    MAX(rs_last_accessed_at) AS rs_last_accessed_at
                FROM resolved
                GROUP BY session_id
            ),
            limited AS MATERIALIZED (
                SELECT
                    session_id,
                    started_at,
                    ended_at,
                    COALESCE(duration_seconds, 0)::bigint AS duration_seconds,
                    page_views,
                    events_count,
                    entry_path,
                    exit_path,
                    referrer,
                    is_bounced,
                    is_engaged,
                    rs_started_at,
                    rs_last_accessed_at,
                    COUNT(*) OVER() AS total_sessions
                FROM session_stats
                -- Events whose session has no request_sessions row (yet) are
                -- neither listed nor counted.
                WHERE session_id IS NOT NULL
                ORDER BY started_at DESC
                LIMIT $2
            )
            SELECT
                l.session_id,
                l.started_at,
                l.ended_at,
                l.duration_seconds,
                l.page_views,
                l.events_count,
                l.entry_path,
                l.exit_path,
                l.referrer,
                l.is_bounced,
                l.is_engaged,
                l.total_sessions,
                (SELECT COUNT(*) FROM proxy_logs pl
                 WHERE pl.session_id = l.session_id
                   AND (pl.project_id IS NULL OR pl.project_id = $3)
                   AND pl.timestamp >= l.rs_started_at - make_interval(hours => $5)
                   AND pl.timestamp <= l.rs_last_accessed_at + make_interval(hours => $5)) AS requests_count
            FROM limited l
            ORDER BY l.started_at DESC
"#;

/// `get_visitor_journey`'s session list (step 1). Parameters: `$1` visitor
/// id, `$2` project, `$3` session limit, `$4` [`earliest_row_time`] of the
/// visitor's `first_seen`.
///
/// Sessions with traffic source context, newest first. Aggregated per raw
/// events.session_id in one pass, then resolved against request_sessions
/// once per distinct raw id (see [`VISITOR_SESSIONS_SQL`]), bounded by the
/// visitor's first_seen.
const VISITOR_JOURNEY_SESSIONS_SQL: &str = r#"            WITH raw_sessions AS (
                SELECT
                    e.session_id AS raw_session_id,
                    MIN(e.timestamp) AS started_at,
                    MAX(e.timestamp) AS ended_at,
                    COUNT(*) FILTER (WHERE e.event_type = 'page_view') AS page_views,
                    COUNT(*) AS events_count,
                    MIN(e.timestamp) FILTER (WHERE e.event_type = 'page_view') AS first_page_view_at,
                    MAX(e.timestamp) FILTER (WHERE e.event_type = 'page_view') AS last_page_view_at,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp ASC)  FILTER (WHERE e.event_type = 'page_view'))[1] AS entry_path,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp DESC) FILTER (WHERE e.event_type = 'page_view'))[1] AS exit_path,
                    COUNT(*) FILTER (WHERE e.event_type NOT IN ('page_view', 'page_leave', 'heartbeat', 'web_vitals')) AS interactions
                FROM events e
                WHERE e.visitor_id = $1
                  AND e.project_id = $2
                  AND e.session_id IS NOT NULL
                  AND e.timestamp >= $4
                GROUP BY e.session_id
            ),
            resolved AS (
                SELECT raw.*, COALESCE(rs_exact.id, rs_v2.id) AS session_id
                FROM raw_sessions raw
                LEFT JOIN LATERAL (
                    SELECT rs.id
                    FROM request_sessions rs
                    WHERE rs.session_id = raw.raw_session_id
                    LIMIT 1
                ) rs_exact ON true
                LEFT JOIN LATERAL (
                    SELECT rs.id
                    FROM request_sessions rs
                    WHERE rs_exact.id IS NULL
                      AND raw.raw_session_id LIKE 'v2|%'
                      AND rs.session_id = split_part(raw.raw_session_id, '|', 2)
                    LIMIT 1
                ) rs_v2 ON true
            ),
            session_data AS (
                SELECT
                    r.session_id,
                    MIN(r.started_at) AS started_at,
                    MAX(r.ended_at) AS ended_at,
                    EXTRACT(EPOCH FROM (MAX(r.ended_at) - MIN(r.started_at)))::bigint AS duration_seconds,
                    SUM(r.page_views)::bigint AS page_views,
                    SUM(r.events_count)::bigint AS events_count,
                    (ARRAY_AGG(r.entry_path ORDER BY r.first_page_view_at ASC) FILTER (WHERE r.first_page_view_at IS NOT NULL))[1] AS entry_path,
                    (ARRAY_AGG(r.exit_path ORDER BY r.last_page_view_at DESC) FILTER (WHERE r.last_page_view_at IS NOT NULL))[1] AS exit_path,
                    SUM(r.page_views) <= 1 AS is_bounced,
                    SUM(r.interactions) > 0 AS is_engaged
                FROM resolved r
                WHERE r.session_id IS NOT NULL
                GROUP BY r.session_id
            )
            SELECT
                sd.session_id,
                sd.started_at,
                sd.ended_at,
                COALESCE(sd.duration_seconds, 0) AS duration_seconds,
                sd.page_views,
                sd.events_count,
                sd.entry_path,
                sd.exit_path,
                rs.referrer,
                rs.referrer_hostname,
                rs.channel,
                rs.utm_source,
                rs.utm_medium,
                rs.utm_campaign,
                sd.is_bounced,
                sd.is_engaged,
                COUNT(*) OVER() AS total_sessions
            FROM session_data sd
            JOIN request_sessions rs ON rs.id = sd.session_id
            ORDER BY sd.started_at DESC
            LIMIT $3
"#;

/// `get_visitor_journey`'s events (step 2): every event of the selected
/// sessions with its computed time_on_page. Parameters: `$1` visitor id, `$2`
/// project, `$3`/`$4` the sessions' first and last event times, then one
/// placeholder per session id from `$5`.
///
/// Materialize all session events in a CTE, then find each page_view's
/// next events with window functions over the session ordered by time
/// (O(n log n); self-joins of the CTE were O(n^2) in a session's
/// events), preserving the exact 3-priority logic:
///   Priority 1: next page_leave for same session+page within 30 min
///   Priority 2: next page_view in same session (any page)
///   Priority 3: the session's own last_accessed_at (covers the exit
///     page, which has no next event), clamped to [1, 1800] seconds
fn visitor_journey_events_sql(session_count: usize) -> String {
    let in_clause = (0..session_count)
        .map(|i| format!("${}", i + 5))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
        WITH selected_sessions AS MATERIALIZED (
            SELECT id, session_id, last_accessed_at
            FROM request_sessions
            WHERE id IN ({in_clause})
        ),
        all_session_events AS MATERIALIZED (
            -- Materialize ALL events for these sessions into a small CTE
            SELECT
                e.id,
                e.event_type,
                COALESCE(e.event_name, e.event_type) as event_name,
                e.timestamp as occurred_at,
                e.page_path,
                e.page_title,
                e.referrer,
                e.scroll_depth,
                e.session_id,
                COALESCE(e.props, e.event_data::jsonb, '{{}}'::jsonb) as event_data,
                COALESCE(s_exact.id, s_v2.id) as rs_id,
                COALESCE(s_exact.last_accessed_at, s_v2.last_accessed_at)
                    as session_last_accessed_at
            FROM events e
            -- Tolerate both bare-UUID and legacy v2|uuid|ts formats in
            -- events.session_id (see VISITOR_SESSIONS_SQL):
            -- two hash lookups into the <= limit_sessions selected rows,
            -- an exact match first.
            LEFT JOIN selected_sessions s_exact ON s_exact.session_id = e.session_id
            LEFT JOIN selected_sessions s_v2
                ON s_exact.id IS NULL
                AND e.session_id LIKE 'v2|%'
                AND s_v2.session_id = split_part(e.session_id, '|', 2)
            WHERE e.visitor_id = $1
              AND e.project_id = $2
              -- The selected sessions' own first and last event times
              -- (step 1), so only the chunks they span are read.
              AND e.timestamp >= $3
              AND e.timestamp <= $4
              AND COALESCE(s_exact.id, s_v2.id) IS NOT NULL
        ),
        -- Earliest page_leave on the same page and earliest page_view
        -- strictly after each event in the same request session (rs_id, so
        -- a session written under both id formats is one timeline, as for
        -- the entry/exit flags): a running MIN over the rows already seen
        -- in descending time order, excluding same-timestamp peers.
        -- The frame head stays at the partition start, so PostgreSQL
        -- accumulates each in a single pass.
        next_events AS (
            SELECT
                ase.*,
                MIN(ase.occurred_at) FILTER (WHERE ase.event_type = 'page_leave') OVER (
                    PARTITION BY ase.rs_id, ase.page_path
                    ORDER BY ase.occurred_at DESC
                    RANGE BETWEEN UNBOUNDED PRECEDING AND '1 microsecond' PRECEDING
                ) as next_page_leave_any_ts,
                MIN(ase.occurred_at) FILTER (WHERE ase.event_type = 'page_view') OVER (
                    PARTITION BY ase.rs_id
                    ORDER BY ase.occurred_at DESC
                    RANGE BETWEEN UNBOUNDED PRECEDING AND '1 microsecond' PRECEDING
                ) as next_page_view_ts
            FROM all_session_events ase
        )
        SELECT
            ase.id,
            ase.event_type,
            ase.event_name,
            ase.occurred_at,
            ase.page_path,
            ase.page_title,
            ase.referrer,
            CASE
                WHEN ase.event_type = 'page_view' THEN
                    -- Priority 3 falls back to the session's own last-activity
                    -- timestamp (not a fixed guess) so the exit page of a
                    -- session -- which by definition has no next event to
                    -- diff against -- still gets a real duration instead of
                    -- NULL. Clamped to [1, 1800] so bounces still render a
                    -- nonzero value and stale/anomalous gaps don't inflate it.
                    LEAST(1800, GREATEST(1, EXTRACT(EPOCH FROM (
                        COALESCE(
                            -- Priority 1 only counts within 30 min.
                            CASE
                                WHEN ase.next_page_leave_any_ts
                                    <= ase.occurred_at + INTERVAL '30 minutes'
                                THEN ase.next_page_leave_any_ts
                            END,
                            ase.next_page_view_ts,
                            ase.session_last_accessed_at,
                            ase.occurred_at + INTERVAL '1 second'
                        ) - ase.occurred_at
                    ))::int))
                ELSE NULL
            END as computed_time_on_page,
            ase.scroll_depth,
            ase.event_data,
            ase.rs_id
        FROM next_events ase
        WHERE ase.event_type NOT IN ('heartbeat', 'web_vitals', 'page_leave')
        ORDER BY ase.rs_id, ase.occurred_at ASC
        "#,
        in_clause = in_clause
    )
}

/// `get_session_events`' two queries: one page of events and the total.
struct SessionEventsQueries {
    page_sql: String,
    page_values: Vec<sea_orm::Value>,
    count_sql: String,
    count_values: Vec<sea_orm::Value>,
}

/// Builds [`SessionEventsQueries`] for `session`'s events in `project_id`,
/// optionally narrowed to an environment and a `(start, end)` range, ordered
/// by time (`"asc"`, otherwise descending).
///
/// The session's events cannot predate its request_sessions row by more than
/// the clock slack, so the lower bound lets TimescaleDB skip every older chunk
/// (compressed chunks cannot use the session index). Only the requested page
/// is read, in index order: no window over the whole session, so a session
/// with millions of events costs O(offset + limit) in the page query. The
/// total is a separate count over the same filter.
fn session_events_queries(
    session: &request_sessions::Model,
    project_id: i32,
    environment_id: Option<i32>,
    (start_date, end_date): (Option<UtcDateTime>, Option<UtcDateTime>),
    limit: i32,
    offset: i32,
    sort_order: Option<&str>,
) -> SessionEventsQueries {
    let mut where_conditions = vec![
        "session_id = $1".to_string(),
        "project_id = $2".to_string(),
        "timestamp >= $3".to_string(),
    ];
    let mut values: Vec<sea_orm::Value> = vec![
        session.session_id.clone().into(),
        project_id.into(),
        earliest_row_time(session.started_at).into(),
    ];
    let mut param_index = 4;

    if let Some(env_id) = environment_id {
        where_conditions.push(format!("environment_id = ${}", param_index));
        values.push(env_id.into());
        param_index += 1;
    }

    if let Some(start) = start_date {
        where_conditions.push(format!("timestamp >= ${}", param_index));
        values.push(start.into());
        param_index += 1;
    }

    if let Some(end) = end_date {
        where_conditions.push(format!("timestamp <= ${}", param_index));
        values.push(end.into());
        param_index += 1;
    }

    let order = match sort_order {
        Some("asc") => "ASC",
        _ => "DESC",
    };
    let where_clause = where_conditions.join(" AND ");
    let page_sql = format!(
        r#"
        SELECT
            COALESCE(event_name, event_type) as event_name,
            timestamp as occurred_at,
            COALESCE(props, event_data::jsonb, '{{}}'::jsonb) as event_data,
            page_path as request_path,
            request_query
        FROM events
        WHERE {where_clause}
        ORDER BY timestamp {order}, id {order}
        LIMIT ${limit_param} OFFSET ${offset_param}
        "#,
        limit_param = param_index,
        offset_param = param_index + 1
    );
    let count_sql = format!("SELECT COUNT(*) as total_count FROM events WHERE {where_clause}");

    let count_values = values.clone();
    values.push(i64::from(limit).into());
    values.push(i64::from(offset).into());

    SessionEventsQueries {
        page_sql,
        page_values: values,
        count_sql,
        count_values,
    }
}

/// The session-event bounds `[$8, $9]` of [`PAGE_PATH_VISITORS_SQL`]: the
/// window widened by the lookback and lookahead. Passed as plain timestamps
/// so TimescaleDB prunes chunks outside them, and saturated so an extreme
/// requested date cannot overflow.
fn page_path_visitors_context_bounds(
    start_date: UtcDateTime,
    end_date: UtcDateTime,
) -> (UtcDateTime, UtcDateTime) {
    let context_start = start_date
        .checked_sub_signed(chrono::Duration::hours(
            PAGE_VISITORS_SESSION_LOOKBACK_HOURS,
        ))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
    let context_end = end_date
        .checked_add_signed(chrono::Duration::minutes(
            PAGE_VISITORS_SESSION_LOOKAHEAD_MINUTES,
        ))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
    (context_start, context_end)
}

/// `get_page_path_detail`'s headline stats: unique visitors, page views and
/// average time on page for the page in the window, and the entry, exit and
/// bounce rates of the sessions that viewed it.
///
/// The cost is bounded by the window, not by the sessions' history:
///   1. target_sessions: the sessions with a non-crawler view of the page in
///      the window (an index range scan on project/type/path/timestamp).
///   2. session_events: only those sessions' page views (any page) and
///      page_leaves of the page, inside [$6, $7] (the window widened by
///      [`page_path_visitors_context_bounds`]) and the project, so
///      TimescaleDB reads only the chunks of the bound. Whether that is one
///      index lookup per session or one hash join over the bound's rows is
///      left to the planner: both are bounded, and forcing per-session
///      lookups costs one index probe per session per chunk, which made
///      this read over 10x slower than the hash join on a 30-day window.
///   3. ranked: one sort of those rows by session and time feeds every window
///      function (O(n log n); the self-joins they replaced were O(n^2) per
///      session).
///   4. session_flow: each session's entry and exit page and page view count
///      come from its rows in the bound, plus two indexed lookups per session
///      for its first page view before $6 and its last page view after $7.
///      Like `get_page_path_visitors`' existence checks, each is bounded by
///      the session's own lifetime in request_sessions widened by `$8` hours
///      ([`ANALYTICS_ROW_CLOCK_SLACK_HOURS`]), so it reads only the chunks the
///      session spans; a session without a request_sessions row falls back to
///      `$8` hours around the bound. Entry, exit and bounce are therefore
///      exact, as if the session's whole life had been read (except for the
///      older keyed sessions described at
///      [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`]).
///   5. Visitor and session totals are aggregated separately and joined as
///      one row each. The previous query cross-joined an inlined session
///      aggregate with every page view, and the planner re-ran that aggregate
///      over all of the sessions' events once per page view.
///
/// time_on_page keeps the 3-priority COALESCE: the next page_leave of the page
/// within 30 min, else the next page view of the session (any page), else a
/// 30s fallback. Values outside (0, 1800) are left out of the average. A next
/// page view past $7 is more than 30 min away, so only its existence matters
/// and it is left out of the average. Only page_leaves of the page are read,
/// since priority 1 only looks at those, and they do not affect the page
/// view counts or the entry/exit pages.
///
/// Entry, exit and bounce are session-level facts derived here: the
/// events.is_entry / is_exit / is_bounce columns are never populated by the
/// ingest path. A bounce is a session that entered on the page and viewed
/// nothing else. Timestamp ties between page views are broken by event id.
///
/// Parameters: `$1` project_id, `$2` page_path, `$3`/`$4` window start
/// (inclusive) / end (exclusive), `$5` environment_id (NULL for all),
/// `$6`/`$7` the session-event bounds from
/// [`page_path_visitors_context_bounds`], `$8`
/// [`ANALYTICS_ROW_CLOCK_SLACK_HOURS`].
const PAGE_PATH_DETAIL_STATS_SQL: &str = r#"
            WITH target_sessions AS MATERIALIZED (
                SELECT DISTINCT e.session_id
                FROM events e
                WHERE e.project_id = $1
                  AND e.event_type = 'page_view'
                  AND e.page_path = $2
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND e.is_crawler = false
                  AND e.session_id IS NOT NULL
                  AND ($5::int IS NULL OR e.environment_id = $5)
            ),
            session_events AS (
                SELECT e.id, e.session_id, e.event_type, e.page_path, e.timestamp, e.visitor_id
                FROM target_sessions ts
                JOIN events e ON e.session_id = ts.session_id
                WHERE e.project_id = $1
                  AND e.timestamp >= $6
                  AND e.timestamp <= $7
                  AND ($5::int IS NULL OR e.environment_id = $5)
                  AND (e.event_type = 'page_view'
                       OR (e.event_type = 'page_leave' AND e.page_path = $2))
            ),
            -- next_*: the earliest page_leave / page view strictly after each
            -- row, a running MIN over the rows already seen in descending time
            -- order, excluding same-timestamp peers. The frame head stays at
            -- the partition start, so PostgreSQL accumulates it in a single
            -- pass. pv_from_last numbers page views from the session's last
            -- one (1 = exit page, pv_total = entry page); its order extends
            -- w_next's, so one sort serves both.
            ranked AS MATERIALIZED (
                SELECT
                    se.session_id,
                    se.event_type,
                    se.page_path,
                    se.timestamp,
                    se.visitor_id,
                    MIN(se.timestamp) FILTER (WHERE se.event_type = 'page_leave')
                        OVER w_next AS next_page_leave_ts,
                    MIN(se.timestamp) FILTER (WHERE se.event_type = 'page_view')
                        OVER w_next AS next_page_view_ts,
                    COUNT(*) FILTER (WHERE se.event_type = 'page_view')
                        OVER w_from_last AS pv_from_last,
                    COUNT(*) FILTER (WHERE se.event_type = 'page_view')
                        OVER (PARTITION BY se.session_id) AS pv_total
                FROM session_events se
                WINDOW
                    w_next AS (
                        PARTITION BY se.session_id
                        ORDER BY se.timestamp DESC
                        RANGE BETWEEN UNBOUNDED PRECEDING AND '1 microsecond' PRECEDING
                    ),
                    w_from_last AS (
                        PARTITION BY se.session_id
                        ORDER BY se.timestamp DESC, se.id DESC
                        ROWS UNBOUNDED PRECEDING
                    )
            ),
            session_flow AS MATERIALIZED (
                SELECT
                    s.session_id,
                    s.pv_total,
                    COALESCE(before_bound.page_path, s.first_in_bound_path) AS entry_path,
                    COALESCE(after_bound.page_path, s.last_in_bound_path) AS exit_path,
                    before_bound.page_path IS NOT NULL AS has_page_view_before_bound,
                    after_bound.page_path IS NOT NULL AS has_page_view_after_bound
                FROM (
                    SELECT
                        r.session_id,
                        MAX(r.pv_total) AS pv_total,
                        MAX(r.page_path) FILTER (
                            WHERE r.event_type = 'page_view' AND r.pv_from_last = r.pv_total
                        ) AS first_in_bound_path,
                        MAX(r.page_path) FILTER (
                            WHERE r.event_type = 'page_view' AND r.pv_from_last = 1
                        ) AS last_in_bound_path
                    FROM ranked r
                    GROUP BY r.session_id
                ) s
                -- Tolerate both bare-UUID and legacy v2|uuid|ts formats in
                -- events.session_id (see VISITOR_SESSIONS_SQL): two
                -- unique-index lookups per session.
                LEFT JOIN request_sessions rs_exact ON rs_exact.session_id = s.session_id
                LEFT JOIN request_sessions rs_v2
                    ON s.session_id LIKE 'v2|%'
                    AND rs_v2.session_id = split_part(s.session_id, '|', 2)
                -- LIMIT keeps each of these one index lookup per session. The
                -- last condition of each only reads the session (a one-time
                -- filter): a session whose lifetime lies inside the bound
                -- skips the lookup, instead of TimescaleDB re-checking every
                -- chunk before (or after) the bound against it.
                LEFT JOIN LATERAL (
                    SELECT earlier.page_path
                    FROM events earlier
                    WHERE earlier.session_id = s.session_id
                      AND earlier.project_id = $1
                      AND earlier.event_type = 'page_view'
                      AND earlier.timestamp < $6
                      AND earlier.timestamp >= COALESCE(rs_exact.started_at, rs_v2.started_at, $6)
                          - make_interval(hours => $8)
                      AND COALESCE(rs_exact.started_at, rs_v2.started_at, $6)
                          - make_interval(hours => $8) < $6
                      AND ($5::int IS NULL OR earlier.environment_id = $5)
                    ORDER BY earlier.timestamp ASC, earlier.id ASC
                    LIMIT 1
                ) before_bound ON true
                LEFT JOIN LATERAL (
                    SELECT later.page_path
                    FROM events later
                    WHERE later.session_id = s.session_id
                      AND later.project_id = $1
                      AND later.event_type = 'page_view'
                      AND later.timestamp > $7
                      AND later.timestamp <= COALESCE(
                              rs_exact.last_accessed_at, rs_v2.last_accessed_at, $7
                          ) + make_interval(hours => $8)
                      AND COALESCE(rs_exact.last_accessed_at, rs_v2.last_accessed_at, $7)
                          + make_interval(hours => $8) > $7
                      AND ($5::int IS NULL OR later.environment_id = $5)
                    ORDER BY later.timestamp DESC, later.id DESC
                    LIMIT 1
                ) after_bound ON true
            ),
            page_stats AS (
                SELECT
                    COUNT(DISTINCT r.visitor_id) AS unique_visitors,
                    COUNT(*) AS total_page_views,
                    AVG(t.seconds) FILTER (WHERE t.seconds > 0 AND t.seconds < 1800)
                        AS avg_time_on_page
                FROM ranked r
                JOIN session_flow sf ON sf.session_id = r.session_id
                CROSS JOIN LATERAL (
                    SELECT CASE
                        -- Priority 1 only counts within 30 min.
                        WHEN r.next_page_leave_ts <= r.timestamp + INTERVAL '30 minutes'
                            THEN EXTRACT(EPOCH FROM (r.next_page_leave_ts - r.timestamp))
                        WHEN r.next_page_view_ts IS NOT NULL
                            THEN EXTRACT(EPOCH FROM (r.next_page_view_ts - r.timestamp))
                        -- The next page view lies past $7, so at least 30 min
                        -- away: outside the average.
                        WHEN sf.has_page_view_after_bound THEN NULL
                        ELSE EXTRACT(EPOCH FROM INTERVAL '30 seconds')
                    END AS seconds
                ) t
                WHERE r.event_type = 'page_view'
                  AND r.page_path = $2
                  AND r.timestamp >= $3
                  AND r.timestamp < $4
            ),
            session_stats AS (
                SELECT
                    COUNT(*) AS total_sessions,
                    COUNT(*) FILTER (WHERE entry_path = $2) AS entry_count,
                    COUNT(*) FILTER (WHERE exit_path = $2) AS exit_count,
                    COUNT(*) FILTER (
                        WHERE entry_path = $2
                          AND pv_total <= 1
                          AND NOT has_page_view_before_bound
                          AND NOT has_page_view_after_bound
                    ) AS bounce_count
                FROM session_flow
            )
            SELECT
                ps.unique_visitors,
                ps.total_page_views,
                COALESCE(ps.avg_time_on_page, 0)::float8 AS avg_time_on_page,
                -- Bounce rate: sessions that entered on this page and viewed
                -- nothing else, over sessions that entered on this page
                -- (the Plausible/GA definition).
                CASE WHEN ss.entry_count > 0
                     THEN ss.bounce_count::float / ss.entry_count::float * 100
                     ELSE 0 END AS bounce_rate,
                CASE WHEN ss.total_sessions > 0
                     THEN ss.entry_count::float / ss.total_sessions::float * 100
                     ELSE 0 END AS entry_rate,
                CASE WHEN ss.total_sessions > 0
                     THEN ss.exit_count::float / ss.total_sessions::float * 100
                     ELSE 0 END AS exit_rate
            FROM page_stats ps
            CROSS JOIN session_stats ss
            "#;

/// A JSON object of at most `max_keys` top-level keys and `max_bytes` bytes.
fn validate_document(
    custom_data: &serde_json::Value,
    max_keys: usize,
    max_bytes: usize,
) -> Result<(), AnalyticsError> {
    let Some(map) = custom_data.as_object() else {
        return Err(AnalyticsError::InvalidEnrichmentData(
            "custom_data must be a JSON object".to_string(),
        ));
    };
    if map.len() > max_keys {
        return Err(AnalyticsError::InvalidEnrichmentData(format!(
            "custom_data would have {} keys after this change; at most {} are allowed",
            map.len(),
            max_keys
        )));
    }
    let size = custom_data.to_string().len();
    if size > max_bytes {
        return Err(AnalyticsError::InvalidEnrichmentData(format!(
            "custom_data would be {} bytes after this change; at most {} are allowed",
            size, max_bytes
        )));
    }
    Ok(())
}

/// Bound what a machine credential baked into a container can store on a
/// visitor row: a JSON object of limited size and key count.
pub fn validate_scoped_enrichment(custom_data: &serde_json::Value) -> Result<(), AnalyticsError> {
    validate_document(
        custom_data,
        MAX_SCOPED_ENRICHMENT_KEYS,
        MAX_SCOPED_ENRICHMENT_BYTES,
    )
}

/// `(top-level key count, serialized bytes)` of a `custom_data` document.
fn custom_data_footprint(custom_data: Option<&serde_json::Value>) -> (usize, usize) {
    custom_data.map_or((0, 0), |value| {
        (
            value.as_object().map_or(0, |map| map.len()),
            value.to_string().len(),
        )
    })
}

/// Validate the document a write would leave behind, against the limits for
/// the caller (`project_scoped` = deployment token, the tighter ones).
///
/// A row that is already over the limit (for example written before the limit
/// existed) stays editable as long as the write does not make it larger, so
/// identity enrichment does not stop working for that visitor; it just can't
/// grow it.
fn validate_merge(
    existing: Option<&serde_json::Value>,
    merged: &serde_json::Value,
    project_scoped: bool,
) -> Result<(), AnalyticsError> {
    let (max_keys, max_bytes) = if project_scoped {
        (MAX_SCOPED_ENRICHMENT_KEYS, MAX_SCOPED_ENRICHMENT_BYTES)
    } else {
        (MAX_ENRICHMENT_KEYS, MAX_ENRICHMENT_BYTES)
    };
    match validate_document(merged, max_keys, max_bytes) {
        Ok(()) => Ok(()),
        Err(error) if merged.is_object() => {
            let (existing_keys, existing_bytes) = custom_data_footprint(existing);
            let (merged_keys, merged_bytes) = custom_data_footprint(Some(merged));
            if merged_keys <= existing_keys && merged_bytes <= existing_bytes {
                Ok(())
            } else if existing_keys > max_keys || existing_bytes > max_bytes {
                Err(AnalyticsError::InvalidEnrichmentData(format!(
                    "this visitor's custom_data is already over the limit ({existing_keys} keys, \
                     {existing_bytes} bytes; at most {max_keys} keys / {max_bytes} bytes) and can \
                     only be reduced, not grown"
                )))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

const ENRICHED_MESSAGE: &str = "Visitor enriched successfully";

fn visitor_not_found(visitor_id: String) -> EnrichVisitorResponse {
    EnrichVisitorResponse {
        success: false,
        visitor_id,
        message: "Visitor not found".to_string(),
        updated: false,
        visitor_row_id: None,
    }
}

/// Merge `incoming` into `existing` at the top level; a `null` value removes
/// the key. Anything that is not an object on both sides is replaced by
/// `incoming`.
fn merge_custom_data(
    existing: Option<&serde_json::Value>,
    incoming: &serde_json::Value,
) -> serde_json::Value {
    match (existing.and_then(|v| v.as_object()), incoming.as_object()) {
        (existing, Some(incoming)) => {
            let mut merged = existing.cloned().unwrap_or_default();
            for (key, value) in incoming {
                if value.is_null() {
                    merged.remove(key);
                } else {
                    merged.insert(key.clone(), value.clone());
                }
            }
            serde_json::Value::Object(merged)
        }
        (_, None) => incoming.clone(),
    }
}

impl AnalyticsService {
    /// Merge `incoming` into the visitor's `custom_data` and persist it.
    ///
    /// Skips the write when nothing changes, so a client that re-sends the same
    /// identity on every page load costs a read, not a row rewrite.
    async fn apply_visitor_enrichment(
        &self,
        visitor_model: temps_entities::visitor::Model,
        project_scoped: bool,
        incoming: serde_json::Value,
        response_visitor_id: String,
    ) -> Result<EnrichVisitorResponse, AnalyticsError> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::visitor;

        let merged = merge_custom_data(visitor_model.custom_data.as_ref(), &incoming);
        let visitor_row_id = visitor_model.id;
        // Nothing to store: an identical document, or removing keys from a row
        // that has no data. The response text is the same for both outcomes, so
        // the body does not say which happened. (Timing, the size-limit errors
        // below and the token's write budget still let a write-only caller
        // infer a little about stored state; that residual is narrowed, not
        // eliminated.)
        let unchanged = visitor_model.custom_data.as_ref() == Some(&merged)
            || (visitor_model.custom_data.is_none()
                && merged.as_object().is_some_and(|map| map.is_empty()));
        if unchanged {
            return Ok(EnrichVisitorResponse {
                success: true,
                visitor_id: response_visitor_id,
                message: ENRICHED_MESSAGE.to_string(),
                updated: false,
                visitor_row_id: Some(visitor_row_id),
            });
        }
        validate_merge(visitor_model.custom_data.as_ref(), &merged, project_scoped)?;

        let mut active_model: visitor::ActiveModel = visitor_model.into();
        active_model.custom_data = Set(Some(merged));
        active_model
            .update(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?;

        Ok(EnrichVisitorResponse {
            success: true,
            visitor_id: response_visitor_id,
            message: ENRICHED_MESSAGE.to_string(),
            updated: true,
            visitor_row_id: Some(visitor_row_id),
        })
    }
}

#[async_trait]
impl Analytics for AnalyticsService {
    /// Get top pages by view count
    async fn get_top_pages(
        &self,
        project_id: i32,
        limit: u64,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
    ) -> Result<Vec<Page>, AnalyticsError> {
        // Build WHERE conditions and values for parameterized query
        let mut where_conditions = vec![
            "project_id = $1".to_string(),
            "event_type = 'page_view'".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let mut param_index = 2;

        if let Some(start) = start_date {
            where_conditions.push(format!("timestamp >= ${}", param_index));
            values.push(start.into());
            param_index += 1;
        }

        if let Some(end) = end_date {
            where_conditions.push(format!("timestamp <= ${}", param_index));
            values.push(end.into());
            param_index += 1;
        }

        let where_clause = where_conditions.join(" AND ");

        // Add limit as parameter
        let sql_query = format!(
            r#"
            SELECT
                page_path as path,
                COUNT(*) as views
            FROM events
            WHERE {}
            GROUP BY page_path
            ORDER BY views DESC
            LIMIT ${}
            "#,
            where_clause, param_index
        );
        values.push((limit as i64).into());

        #[derive(FromQueryResult)]
        struct PageResult {
            path: String,
            views: i64,
        }

        let pages = PageResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(pages
            .into_iter()
            .map(|p| Page {
                path: p.path,
                views: p.views as u64,
            })
            .collect())
    }

    /// Get event counts
    async fn get_events_count(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        limit: Option<i32>,
        custom_events_only: Option<bool>,
        breakdown: Option<crate::types::requests::EventBreakdown>,
    ) -> Result<Vec<EventCount>, AnalyticsError> {
        use crate::types::requests::EventBreakdown;

        // Build WHERE conditions and values for parameterized query
        let mut where_conditions = vec![
            "e.project_id = $1".to_string(),
            "e.timestamp >= $2".to_string(),
            "e.timestamp <= $3".to_string(),
            "e.event_name IS NOT NULL".to_string(),
            // Traffic-filter exclusions. Not every bot verdict: this report
            // never left those out, and events sent without a user agent
            // (server-side ingestion) are stored as the bot `unknown`.
            temps_entities::events::not_excluded_by_traffic_filter("e"),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_index = 4;

        // Default to true - only return custom events by default
        let filter_custom_only = custom_events_only.unwrap_or(true);

        if filter_custom_only {
            // Exclude system events like page_view, page_leave, heartbeat
            where_conditions.push(
                "COALESCE(e.event_name, e.event_type) NOT IN ('page_view', 'page_leave', 'heartbeat')"
                    .to_string(),
            );
        }

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("e.environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        let limit_val = limit.unwrap_or(20).min(100);

        // Build GROUP BY clause based on breakdown option
        let (group_by_field, select_field) = match breakdown {
            Some(EventBreakdown::Country) => (
                "COALESCE(ig.country, 'Unknown')",
                "COALESCE(ig.country, 'Unknown') as event_name",
            ),
            Some(EventBreakdown::Region) => (
                "COALESCE(ig.region, 'Unknown')",
                "COALESCE(ig.region, 'Unknown') as event_name",
            ),
            Some(EventBreakdown::City) => (
                "COALESCE(ig.city, 'Unknown')",
                "COALESCE(ig.city, 'Unknown') as event_name",
            ),
            None => (
                "COALESCE(e.event_name, e.event_type)",
                "COALESCE(e.event_name, e.event_type) as event_name",
            ),
        };

        let where_clause = where_conditions.join(" AND ");

        let sql_query = format!(
            r#"
            WITH event_counts AS (
                SELECT
                    {},
                    COUNT(*) as count
                FROM events e
                LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
                WHERE {}
                GROUP BY {}
            ),
            total AS (
                SELECT SUM(count) as total_count
                FROM event_counts
            )
            SELECT
                ec.event_name,
                ec.count,
                CASE WHEN t.total_count > 0
                     THEN (ec.count::float / t.total_count::float * 100)
                     ELSE 0 END as percentage
            FROM event_counts ec
            CROSS JOIN total t
            ORDER BY ec.count DESC
            LIMIT ${}
            "#,
            select_field, where_clause, group_by_field, param_index
        );
        values.push((limit_val as i64).into());

        #[derive(FromQueryResult)]
        struct EventResult {
            event_name: String,
            count: i64,
            percentage: f64,
        }

        let results = EventResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(results
            .into_iter()
            .map(|r| EventCount {
                event_name: r.event_name,
                count: r.count,
                percentage: r.percentage,
            })
            .collect())
    }

    /// Get visitors list
    async fn get_visitors(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        include_crawlers: Option<bool>,
        limit: Option<i32>,
        offset: Option<i32>,
        has_activity_only: Option<bool>,
        segment: crate::types::requests::VisitorSegmentFilters,
    ) -> Result<VisitorsResponse, AnalyticsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec!["v.project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let mut param_index = 2;

        // Add environment filter if provided
        if let Some(env_id) = environment_id {
            where_conditions.push(format!("v.environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        // Add crawler filter if requested
        if include_crawlers == Some(false) {
            where_conditions.push("v.is_crawler = false".to_string());
        }

        // Add has_activity filter to exclude ghost visitors
        if has_activity_only == Some(true) {
            where_conditions.push("v.has_activity = true".to_string());
        }

        // Add date range filter - check last_seen is within range
        where_conditions.push(format!("v.last_seen >= ${}", param_index));
        values.push(start_date.into());
        param_index += 1;

        where_conditions.push(format!("v.last_seen <= ${}", param_index));
        values.push(end_date.into());
        param_index += 1;

        // ─── Segment filters ────────────────────────────────────────────────
        // Visitor-row filters resolve against the visitor table directly so we
        // can keep the count/list queries on the same plan. Event-row filters
        // (browser/OS/device/UTM/event_name/language) need at least one event
        // in the range that matches — expressed as a single EXISTS subquery
        // so we don't multiply rows.

        // Visitor-side: country / region / city require the ip_geolocations
        // LEFT JOIN we already do in the list query. For the count query we
        // need to make sure ig.* is reachable — we handle that by joining in
        // both queries below when a geo filter is present.
        let needs_geo_join = segment.filter_country.is_some()
            || segment.filter_region.is_some()
            || segment.filter_city.is_some();

        if let Some(country) = &segment.filter_country {
            where_conditions.push(format!("ig.country = ${}", param_index));
            values.push(country.clone().into());
            param_index += 1;
        }
        if let Some(region) = &segment.filter_region {
            where_conditions.push(format!("ig.region = ${}", param_index));
            values.push(region.clone().into());
            param_index += 1;
        }
        if let Some(city) = &segment.filter_city {
            where_conditions.push(format!("ig.city = ${}", param_index));
            values.push(city.clone().into());
            param_index += 1;
        }

        // Visitor-side: first-touch channel / referrer.
        // "Direct" referrer is stored as NULL in the visitor table.
        if let Some(channel) = &segment.filter_channel {
            where_conditions.push(format!("v.first_channel = ${}", param_index));
            values.push(channel.clone().into());
            param_index += 1;
        }
        if let Some(referrer) = &segment.filter_referrer {
            if referrer == "Direct" {
                where_conditions.push("v.first_referrer_hostname IS NULL".to_string());
            } else {
                where_conditions.push(format!("v.first_referrer_hostname = ${}", param_index));
                values.push(referrer.clone().into());
                param_index += 1;
            }
        }

        let limit_val = limit.unwrap_or(50).min(100);
        let offset_val = offset.unwrap_or(0);

        let where_clause = where_conditions.join(" AND ");

        // FROM clause needs the geo join when a geo filter is in play; the
        // list query already uses LEFT JOIN ig so reuse the same join here.
        let geo_join = if needs_geo_join {
            "LEFT JOIN ip_geolocations ig ON v.ip_address_id = ig.id"
        } else {
            ""
        };

        // Count total before applying limit/offset
        let count_sql = format!(
            r#"
            SELECT COUNT(*) as total
            FROM visitor v
            {}
            WHERE {}
            "#,
            geo_join, where_clause
        );

        #[derive(FromQueryResult)]
        struct CountResult {
            total: i64,
        }

        let count_results = CountResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &count_sql,
            values.clone(),
        ))
        .all(self.db.as_ref())
        .await?;

        let total_count = count_results.first().map(|r| r.total).unwrap_or(0);

        // Query visitors with geolocation data and most recent page.
        // The LATERAL join uses idx_events_visitor_timestamp (visitor_id, timestamp DESC)
        // for an efficient index scan (1 row per visitor) instead of a full hypertable scan.
        // The visitor listing uses idx_visitor_project_last_seen for ORDER BY last_seen DESC.
        let sql_query = format!(
            r#"
            SELECT
                v.id,
                v.visitor_id,
                v.project_id,
                v.environment_id,
                v.first_seen,
                v.last_seen,
                v.user_agent,
                v.ip_address_id,
                v.is_crawler,
                v.crawler_name,
                v.custom_data,
                ig.ip_address,
                ig.latitude,
                ig.longitude,
                ig.region,
                ig.city,
                ig.country,
                ig.country_code,
                ig.timezone,
                ig.is_eu,
                last_event.page_path as current_page,
                v.first_referrer,
                v.first_referrer_hostname,
                v.first_channel
            FROM visitor v
            LEFT JOIN ip_geolocations ig ON v.ip_address_id = ig.id
            LEFT JOIN LATERAL (
                SELECT e.page_path
                FROM events e
                WHERE e.visitor_id = v.id
                ORDER BY e.timestamp DESC
                LIMIT 1
            ) last_event ON true
            WHERE {}
            ORDER BY v.last_seen DESC
            LIMIT ${} OFFSET ${}
            "#,
            where_clause,
            param_index,
            param_index + 1
        );

        // Add LIMIT and OFFSET as parameters
        values.push((limit_val as i64).into());
        values.push((offset_val as i64).into());

        #[derive(FromQueryResult)]
        struct VisitorResult {
            id: i32,
            visitor_id: String,
            project_id: i32,
            environment_id: i32,
            first_seen: UtcDateTime,
            last_seen: UtcDateTime,
            user_agent: Option<String>,
            ip_address_id: Option<i32>,
            is_crawler: bool,
            crawler_name: Option<String>,
            custom_data: Option<serde_json::Value>,
            ip_address: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
            region: Option<String>,
            city: Option<String>,
            country: Option<String>,
            country_code: Option<String>,
            timezone: Option<String>,
            is_eu: Option<bool>,
            current_page: Option<String>,
            first_referrer: Option<String>,
            first_referrer_hostname: Option<String>,
            first_channel: Option<String>,
        }

        let results = VisitorResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        let visitors = results
            .into_iter()
            .map(|r| crate::types::responses::VisitorInfo {
                id: r.id,
                visitor_id: r.visitor_id,
                project_id: r.project_id,
                environment_id: r.environment_id,
                first_seen: r.first_seen,
                last_seen: r.last_seen,
                user_agent: r.user_agent,
                ip_address_id: r.ip_address_id,
                is_crawler: r.is_crawler,
                crawler_name: r.crawler_name,
                custom_data: r.custom_data,
                ip_address: r.ip_address,
                latitude: r.latitude,
                longitude: r.longitude,
                region: r.region,
                city: r.city,
                country: r.country,
                country_code: r.country_code,
                timezone: r.timezone,
                is_eu: r.is_eu,
                current_page: r.current_page,
                first_referrer: r.first_referrer,
                first_referrer_hostname: r.first_referrer_hostname,
                first_channel: r.first_channel,
            })
            .collect();

        Ok(VisitorsResponse {
            visitors,
            total_count,
            filtered_count: total_count,
        })
    }

    async fn get_visitor_facets(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        include_crawlers: Option<bool>,
        has_activity_only: Option<bool>,
        per_facet_limit: Option<i32>,
        segment: crate::types::requests::VisitorSegmentFilters,
    ) -> Result<VisitorFacets, AnalyticsError> {
        let per_facet_limit = per_facet_limit.unwrap_or(50).clamp(1, 200);

        // Every dimension aggregates the visitor pool with all *other*
        // segment filters applied, so a selected dimension doesn't collapse
        // its own dropdown to a single option.
        //
        // Only visitor-row dimensions (country/region/city/channel/referrer)
        // are supported on purpose: they aggregate directly off `visitor` +
        // `ip_geolocations`, which are small relative to events and have
        // proper indexes. Adding event-row dimensions would pull in the
        // events hypertable and reintroduce 100+ ms of per-query cost.

        macro_rules! without {
            ($field:ident) => {{
                let mut s = segment.clone();
                s.$field = None;
                s
            }};
        }

        let scope = FacetScope {
            start_date,
            end_date,
            project_id,
            environment_id,
            include_crawlers,
            has_activity_only,
            limit: per_facet_limit,
            _marker: std::marker::PhantomData,
        };

        // Fan the 5 visitor-row queries out concurrently — each is fast
        // (~5–15 ms) but running them in parallel still meaningfully cuts
        // wall-clock vs awaiting sequentially.
        let seg_country = without!(filter_country);
        let seg_region = without!(filter_region);
        let seg_city = without!(filter_city);
        let seg_channel = without!(filter_channel);
        let seg_referrer = without!(filter_referrer);

        let (country, region, city, channel, referrer) = tokio::try_join!(
            self.facet_visitor_dimension(
                "ig.country",
                Some("ig.country_code"),
                FacetGeoMode::Always,
                &scope,
                &seg_country,
            ),
            self.facet_visitor_dimension(
                "ig.region",
                None,
                FacetGeoMode::Always,
                &scope,
                &seg_region,
            ),
            self.facet_visitor_dimension("ig.city", None, FacetGeoMode::Always, &scope, &seg_city,),
            self.facet_visitor_dimension(
                "v.first_channel",
                None,
                FacetGeoMode::IfFiltered,
                &scope,
                &seg_channel,
            ),
            // Referrer: NULL means "Direct" — the SQL collapses it to that
            // literal so the UI doesn't have to special-case empty rows.
            self.facet_visitor_dimension(
                "COALESCE(v.first_referrer_hostname, 'Direct')",
                None,
                FacetGeoMode::IfFiltered,
                &scope,
                &seg_referrer,
            ),
        )?;

        Ok(VisitorFacets {
            country,
            region,
            city,
            channel,
            referrer,
        })
    }

    /// Get visitor basic info from database
    async fn get_visitor_info(
        &self,
        visitor_id: i32,
    ) -> Result<Option<responses::VisitorRecord>, AnalyticsError> {
        use temps_entities::visitor;

        let visitor = visitor::Entity::find()
            .filter(visitor::Column::Id.eq(visitor_id))
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::DatabaseError)?;

        Ok(visitor.map(|v| responses::VisitorRecord {
            id: v.id,
            visitor_id: v.visitor_id,
            project_id: v.project_id,
            custom_data: v.custom_data,
            created_at: v.first_seen,
        }))
    }

    /// Get visitor statistics
    async fn get_visitor_statistics(
        &self,
        visitor_id: i32,
    ) -> Result<Option<responses::VisitorStats>, AnalyticsError> {
        // First check if visitor exists
        use temps_entities::visitor;

        let visitor = visitor::Entity::find()
            .filter(visitor::Column::Id.eq(visitor_id))
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::DatabaseError)?;

        if visitor.is_none() {
            return Ok(None);
        }

        // Get basic statistics.
        // Bounce and session duration are derived from per-session page_view
        // counts and event timestamps at query time — the events.is_bounce and
        // events.time_on_page columns are never populated by the ingest path,
        // so aggregating them would always return 0.
        let stats_query = r#"
            WITH session_stats AS (
                SELECT
                    session_id,
                    COUNT(*) FILTER (WHERE event_type = 'page_view') as page_views,
                    EXTRACT(EPOCH FROM (MAX(timestamp) - MIN(timestamp))) as duration_seconds
                FROM events
                WHERE visitor_id = $1
                  AND session_id IS NOT NULL
                GROUP BY session_id
            ),
            visitor_stats AS (
                SELECT
                    MIN(timestamp) as first_seen,
                    MAX(timestamp) as last_seen,
                    COUNT(DISTINCT session_id) as total_sessions,
                    COUNT(*) FILTER (WHERE event_type = 'page_view') as total_page_views,
                    COUNT(*) as total_events,
                    COUNT(*) FILTER (WHERE event_type NOT IN ('page_view', 'page_leave')) as engagement_events
                FROM events
                WHERE visitor_id = $1
            )
            SELECT
                vs.first_seen,
                vs.last_seen,
                vs.total_sessions,
                vs.total_page_views,
                vs.total_events,
                COALESCE((SELECT AVG(duration_seconds) FROM session_stats), 0)::float8
                    as average_session_duration,
                COALESCE(
                    (SELECT (COUNT(*) FILTER (WHERE page_views <= 1))::float
                            / NULLIF(COUNT(*), 0)::float * 100
                     FROM session_stats), 0)::float8 as bounce_rate,
                CASE WHEN vs.total_events > 0
                     THEN vs.engagement_events::float / vs.total_events::float * 100
                     ELSE 0 END as engagement_rate
            FROM visitor_stats vs
            "#;

        #[derive(FromQueryResult)]
        struct StatsResult {
            first_seen: UtcDateTime,
            last_seen: UtcDateTime,
            total_sessions: i64,
            total_page_views: i64,
            total_events: i64,
            average_session_duration: f64,
            bounce_rate: f64,
            engagement_rate: f64,
        }

        let stats = StatsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            stats_query,
            vec![visitor_id.into()],
        ))
        .one(self.db.as_ref())
        .await?;

        if let Some(s) = stats {
            // Get top pages
            let pages_query = r#"
                SELECT page_path as path, COUNT(*) as visits
                FROM events
                WHERE visitor_id = $1 AND event_type = 'page_view'
                GROUP BY page_path
                ORDER BY visits DESC
                LIMIT 10
                "#;

            #[derive(FromQueryResult)]
            struct PageResult {
                path: String,
                visits: i64,
            }

            let top_pages = PageResult::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                pages_query,
                vec![visitor_id.into()],
            ))
            .all(self.db.as_ref())
            .await?;

            // Get top referrers
            let referrers_query = r#"
                SELECT DISTINCT referrer
                FROM events
                WHERE visitor_id = $1
                    AND referrer IS NOT NULL AND referrer != ''
                LIMIT 10
                "#;

            #[derive(FromQueryResult)]
            struct ReferrerResult {
                referrer: String,
            }

            let referrers = ReferrerResult::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                referrers_query,
                vec![visitor_id.into()],
            ))
            .all(self.db.as_ref())
            .await?;

            // Get devices used
            let devices_query = r#"
                SELECT DISTINCT COALESCE(browser, 'Unknown') || ' on ' || COALESCE(operating_system, 'Unknown') as device
                FROM events
                WHERE visitor_id = $1
                LIMIT 10
                "#;

            #[derive(FromQueryResult)]
            struct DeviceResult {
                device: String,
            }

            let devices = DeviceResult::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                devices_query,
                vec![visitor_id.into()],
            ))
            .all(self.db.as_ref())
            .await?;

            // Get locations
            let locations_query = r#"
                SELECT DISTINCT
                    ig.country,
                    ig.city,
                    ig.region
                FROM events e
                LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
                WHERE e.visitor_id = $1 AND ig.id IS NOT NULL
                LIMIT 10
                "#;

            #[derive(FromQueryResult)]
            struct LocationResult {
                country: Option<String>,
                city: Option<String>,
                region: Option<String>,
            }

            let locations = LocationResult::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                locations_query,
                vec![visitor_id.into()],
            ))
            .all(self.db.as_ref())
            .await?;

            Ok(Some(responses::VisitorStats {
                visitor_id,
                first_seen: s.first_seen,
                last_seen: s.last_seen,
                total_sessions: s.total_sessions,
                total_page_views: s.total_page_views,
                total_events: s.total_events,
                average_session_duration: s.average_session_duration,
                bounce_rate: s.bounce_rate,
                engagement_rate: s.engagement_rate,
                top_pages: top_pages
                    .into_iter()
                    .map(|p| responses::PageVisit {
                        path: p.path,
                        visits: p.visits,
                    })
                    .collect(),
                top_referrers: referrers.into_iter().map(|r| r.referrer).collect(),
                devices_used: devices.into_iter().map(|d| d.device).collect(),
                locations: locations
                    .into_iter()
                    .map(|l| responses::LocationInfo {
                        country: l.country,
                        city: l.city,
                        region: l.region,
                    })
                    .collect(),
            }))
        } else {
            Ok(None)
        }
    }

    /// Get visitor details by ID
    async fn get_visitor_details_by_id(
        &self,
        visitor_id: i32,
    ) -> Result<Option<VisitorDetails>, AnalyticsError> {
        let sql_query = r#"
            SELECT
                v.id,
                v.visitor_id,
                v.project_id,
                v.environment_id,
                v.first_seen,
                v.last_seen,
                v.user_agent,
                v.ip_address_id,
                v.is_crawler,
                v.crawler_name,
                v.custom_data,
                ig.ip_address,
                ig.latitude,
                ig.longitude,
                ig.region,
                ig.city,
                ig.country,
                ig.country_code,
                ig.timezone,
                ig.is_eu,
                ig.asn_number,
                ig.asn AS asn_org,
                v.first_referrer,
                v.first_referrer_hostname,
                v.first_channel
            FROM visitor v
            LEFT JOIN ip_geolocations ig ON v.ip_address_id = ig.id
            WHERE v.id = $1
            "#;

        #[derive(FromQueryResult)]
        struct DetailResult {
            id: i32,
            visitor_id: String,
            project_id: i32,
            environment_id: i32,
            first_seen: UtcDateTime,
            last_seen: UtcDateTime,
            user_agent: Option<String>,
            ip_address_id: Option<i32>,
            is_crawler: bool,
            crawler_name: Option<String>,
            custom_data: Option<serde_json::Value>,
            ip_address: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
            region: Option<String>,
            city: Option<String>,
            country: Option<String>,
            country_code: Option<String>,
            timezone: Option<String>,
            is_eu: Option<bool>,
            asn_number: Option<i32>,
            asn_org: Option<String>,
            first_referrer: Option<String>,
            first_referrer_hostname: Option<String>,
            first_channel: Option<String>,
        }

        let result = DetailResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            vec![visitor_id.into()],
        ))
        .one(self.db.as_ref())
        .await?;

        Ok(result.map(|r| VisitorDetails {
            id: r.id,
            visitor_id: r.visitor_id,
            project_id: r.project_id,
            environment_id: r.environment_id,
            first_seen: r.first_seen,
            last_seen: r.last_seen,
            user_agent: r.user_agent,
            ip_address_id: r.ip_address_id,
            is_crawler: r.is_crawler,
            crawler_name: r.crawler_name,
            custom_data: r.custom_data,
            ip_address: r.ip_address,
            latitude: r.latitude,
            longitude: r.longitude,
            region: r.region,
            city: r.city,
            country: r.country,
            country_code: r.country_code,
            timezone: r.timezone,
            is_eu: r.is_eu,
            asn_number: r.asn_number,
            asn_org: r.asn_org,
            first_referrer: r.first_referrer,
            first_referrer_hostname: r.first_referrer_hostname,
            first_channel: r.first_channel,
        }))
    }

    /// Get visitor sessions by ID
    async fn get_visitor_sessions_by_id(
        &self,
        visitor_id: i32,
        limit: Option<i32>,
    ) -> Result<Option<VisitorSessionsResponse>, AnalyticsError> {
        let limit_val = limit.unwrap_or(100).min(500);

        // First check if visitor exists
        let visitor = visitor::Entity::find_by_id(visitor_id)
            .one(self.db.as_ref())
            .await?;

        let Some(visitor) = visitor else {
            return Ok(None);
        };

        #[derive(FromQueryResult)]
        struct SessionResult {
            // The query drops sessions without a request_sessions row; kept
            // nullable so a NULL can never fail the decode of the whole page.
            session_id: Option<i32>,
            started_at: UtcDateTime,
            ended_at: Option<UtcDateTime>,
            duration_seconds: i64,
            page_views: i64,
            events_count: i64,
            requests_count: i64,
            entry_path: Option<String>,
            exit_path: Option<String>,
            referrer: Option<String>,
            is_bounced: bool,
            is_engaged: bool,
            total_sessions: i64,
        }

        let results = SessionResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            VISITOR_SESSIONS_SQL,
            vec![
                visitor_id.into(),
                (limit_val as i64).into(),
                visitor.project_id.into(),
                earliest_row_time(visitor.first_seen).into(),
                ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let total_sessions = results.first().map(|r| r.total_sessions).unwrap_or(0);
        let sessions = results
            .into_iter()
            .filter_map(|r| {
                Some(crate::types::responses::SessionSummary {
                    session_id: r.session_id?,
                    started_at: r.started_at,
                    ended_at: r.ended_at,
                    duration_seconds: r.duration_seconds,
                    page_views: r.page_views,
                    events_count: r.events_count,
                    requests_count: r.requests_count,
                    entry_path: r.entry_path,
                    exit_path: r.exit_path,
                    referrer: r.referrer,
                    is_bounced: r.is_bounced,
                    is_engaged: r.is_engaged,
                })
            })
            .collect();

        Ok(Some(VisitorSessionsResponse {
            visitor_id: visitor.visitor_id,
            sessions,
            total_sessions,
        }))
    }

    /// Get the complete visitor journey: all events across all sessions, grouped by session
    async fn get_visitor_journey(
        &self,
        visitor_id: i32,
        project_id: i32,
        limit_sessions: Option<i32>,
    ) -> Result<Option<crate::types::responses::VisitorJourneyResponse>, AnalyticsError> {
        // Check if visitor exists
        let visitor = visitor::Entity::find_by_id(visitor_id)
            .one(self.db.as_ref())
            .await?;

        let Some(visitor) = visitor else {
            return Ok(None);
        };

        let limit_sessions = limit_sessions.unwrap_or(50).min(100) as i64;

        // Step 1: sessions with traffic source context, newest first.

        #[derive(FromQueryResult)]
        struct JourneySessionRow {
            session_id: i32,
            started_at: UtcDateTime,
            ended_at: Option<UtcDateTime>,
            duration_seconds: i64,
            page_views: i64,
            events_count: i64,
            entry_path: Option<String>,
            exit_path: Option<String>,
            referrer: Option<String>,
            referrer_hostname: Option<String>,
            channel: Option<String>,
            utm_source: Option<String>,
            utm_medium: Option<String>,
            utm_campaign: Option<String>,
            is_bounced: bool,
            is_engaged: bool,
            total_sessions: i64,
        }

        let session_rows = JourneySessionRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            VISITOR_JOURNEY_SESSIONS_SQL,
            vec![
                visitor_id.into(),
                project_id.into(),
                limit_sessions.into(),
                earliest_row_time(visitor.first_seen).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        if session_rows.is_empty() {
            return Ok(Some(crate::types::responses::VisitorJourneyResponse {
                visitor_id,
                total_sessions: 0,
                total_events: 0,
                sessions: vec![],
            }));
        }

        let total_sessions = session_rows.first().map(|r| r.total_sessions).unwrap_or(0);

        // Step 2: Collect all session IDs, then fetch events for all sessions in one query
        let session_ids: Vec<i32> = session_rows.iter().map(|r| r.session_id).collect();

        let events_sql = visitor_journey_events_sql(session_ids.len());

        #[derive(FromQueryResult)]
        struct JourneyEventRow {
            id: i64,
            event_type: String,
            event_name: String,
            occurred_at: UtcDateTime,
            page_path: Option<String>,
            page_title: Option<String>,
            referrer: Option<String>,
            computed_time_on_page: Option<i32>,
            scroll_depth: Option<i32>,
            event_data: serde_json::Value,
            rs_id: i32,
        }

        // Every event of the selected sessions lies within their first and
        // last event times, which step 1 computed from the same rows.
        let events_from = session_rows.iter().map(|r| r.started_at).min();
        let events_to = session_rows
            .iter()
            .map(|r| r.ended_at.unwrap_or(r.started_at))
            .max();
        let mut event_values: Vec<sea_orm::Value> = vec![
            visitor_id.into(),
            project_id.into(),
            events_from.into(),
            events_to.into(),
        ];
        for id in &session_ids {
            event_values.push((*id).into());
        }

        let event_rows = JourneyEventRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &events_sql,
            event_values,
        ))
        .all(self.db.as_ref())
        .await?;

        // Step 3: Group events by session_id
        let mut events_by_session: std::collections::HashMap<
            i32,
            Vec<crate::types::responses::JourneyEvent>,
        > = std::collections::HashMap::new();

        let mut total_events: i64 = 0;
        for row in event_rows {
            total_events += 1;
            let time_on_page = if row.event_type == "page_view" {
                // SQL already clamps to [1, 1800] for page_view rows, so this is
                // never NULL/out-of-range here -- no further filtering needed.
                row.computed_time_on_page
            } else {
                None
            };

            let event_data = if row.event_data == serde_json::json!({}) {
                None
            } else {
                Some(row.event_data)
            };

            events_by_session.entry(row.rs_id).or_default().push(
                crate::types::responses::JourneyEvent {
                    id: row.id,
                    event_type: row.event_type,
                    event_name: row.event_name,
                    occurred_at: row.occurred_at,
                    page_path: row.page_path,
                    page_title: row.page_title,
                    referrer: row.referrer,
                    time_on_page,
                    is_entry: false,
                    is_exit: false,
                    is_bounce: false,
                    session_page_number: None,
                    scroll_depth: row.scroll_depth,
                    event_data,
                },
            );
        }

        // Derive session-flow flags from event order within each session. The
        // events.is_entry / is_exit / is_bounce / session_page_number columns
        // are never populated by the ingest path, so they are computed here.
        // Events are already ordered by occurred_at ASC per session (SQL
        // ORDER BY ase.rs_id, ase.occurred_at ASC).
        for events in events_by_session.values_mut() {
            let pv_indices: Vec<usize> = events
                .iter()
                .enumerate()
                .filter(|(_, e)| e.event_type == "page_view")
                .map(|(i, _)| i)
                .collect();
            let pv_count = pv_indices.len();
            for (n, &idx) in pv_indices.iter().enumerate() {
                events[idx].session_page_number = Some((n + 1) as i32);
                events[idx].is_entry = n == 0;
                events[idx].is_exit = n + 1 == pv_count;
            }
            let bounced = pv_count <= 1;
            for event in events.iter_mut() {
                event.is_bounce = bounced;
            }
        }

        // Step 4: Build the response - sessions ordered newest first with their events
        let sessions = session_rows
            .into_iter()
            .map(|s| {
                let events = events_by_session.remove(&s.session_id).unwrap_or_default();

                crate::types::responses::JourneySession {
                    session_id: s.session_id,
                    started_at: s.started_at,
                    ended_at: s.ended_at,
                    duration_seconds: s.duration_seconds,
                    page_views: s.page_views,
                    events_count: s.events_count,
                    entry_path: s.entry_path,
                    exit_path: s.exit_path,
                    referrer: s.referrer,
                    referrer_hostname: s.referrer_hostname,
                    channel: s.channel,
                    utm_source: s.utm_source,
                    utm_medium: s.utm_medium,
                    utm_campaign: s.utm_campaign,
                    is_bounced: s.is_bounced,
                    is_engaged: s.is_engaged,
                    events,
                }
            })
            .collect();

        Ok(Some(crate::types::responses::VisitorJourneyResponse {
            visitor_id,
            total_sessions,
            total_events,
            sessions,
        }))
    }

    /// Get session details
    async fn get_session_details(
        &self,
        session_id: i32,
        _project_id: i32,
        _environment_id: Option<i32>,
    ) -> Result<Option<SessionDetails>, AnalyticsError> {
        // Simplified query with only aggregates
        let query = r#"
            SELECT
                rs.id as session_id,
                COALESCE(rs.visitor_id::text, '0') as visitor_id,
                rs.started_at,
                rs.last_accessed_at as ended_at,
                EXTRACT(EPOCH FROM (rs.last_accessed_at - rs.started_at))::bigint as duration_seconds,
                rs.referrer,

                -- Get entry and exit paths.
                -- Each subquery matches both bare-UUID (new events) and the
                -- legacy v2|uuid|ts format written by older server versions.
                -- rs.session_id is always a bare UUID; e.session_id may be
                -- either format (see normalise_session_cookie fix in
                -- temps-core::request_metadata).
                (SELECT e.page_path FROM events e
                    WHERE (e.session_id = rs.session_id
                           OR (e.session_id LIKE 'v2|%' AND split_part(e.session_id, '|', 2) = rs.session_id))
                    ORDER BY e.timestamp ASC LIMIT 1) as entry_path,
                (SELECT e.page_path FROM events e
                    WHERE (e.session_id = rs.session_id
                           OR (e.session_id LIKE 'v2|%' AND split_part(e.session_id, '|', 2) = rs.session_id))
                    ORDER BY e.timestamp DESC LIMIT 1) as exit_path,

                -- Count page views
                (SELECT COUNT(*) FROM events e
                    WHERE (e.session_id = rs.session_id
                           OR (e.session_id LIKE 'v2|%' AND split_part(e.session_id, '|', 2) = rs.session_id))
                    AND COALESCE(e.event_name, e.event_type, 'page_view') = 'page_view') as page_views,

                -- Calculate bounce (1 or fewer page views)
                (SELECT COUNT(*) FROM events e
                    WHERE (e.session_id = rs.session_id
                           OR (e.session_id LIKE 'v2|%' AND split_part(e.session_id, '|', 2) = rs.session_id))
                    AND COALESCE(e.event_name, e.event_type, 'page_view') = 'page_view') <= 1 as is_bounced,

                -- Engaged if the visitor spent >= 10s of measured time, OR
                -- fired a genuine interaction event. Auto-fired view events
                -- (page_view, page_leave, *_viewed) are excluded — they fire
                -- from intersection observers for bots too.
                (
                    EXTRACT(EPOCH FROM (rs.last_accessed_at - rs.started_at)) >= 10
                    OR (SELECT COUNT(*) > 0 FROM events e
                        WHERE (e.session_id = rs.session_id
                               OR (e.session_id LIKE 'v2|%' AND split_part(e.session_id, '|', 2) = rs.session_id))
                        AND COALESCE(e.event_name, e.event_type, '') NOT IN ('page_view', 'page_leave', '')
                        AND COALESCE(e.event_name, e.event_type, '') NOT LIKE '%\_viewed' ESCAPE '\')
                ) as is_engaged

            FROM request_sessions rs
            WHERE rs.id = $1
        "#;

        #[derive(FromQueryResult)]
        struct SessionDetailsResult {
            session_id: i32,
            visitor_id: String,
            started_at: UtcDateTime,
            ended_at: Option<UtcDateTime>,
            duration_seconds: i64,
            entry_path: Option<String>,
            exit_path: Option<String>,
            referrer: Option<String>,
            page_views: i64,
            is_bounced: bool,
            is_engaged: bool,
        }

        let result = SessionDetailsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            query,
            vec![session_id.into()],
        ))
        .one(self.db.as_ref())
        .await?;

        if let Some(row) = result {
            Ok(Some(SessionDetails {
                session_id: row.session_id,
                visitor_id: row.visitor_id,
                started_at: row.started_at,
                ended_at: row.ended_at,
                duration_seconds: row.duration_seconds,
                entry_path: row.entry_path,
                exit_path: row.exit_path,
                referrer: row.referrer.filter(|r| !r.is_empty()),
                is_bounced: row.is_bounced,
                is_engaged: row.is_engaged,
                page_views: row.page_views,
            }))
        } else {
            Ok(None)
        }
    }

    /// Get session events
    async fn get_session_events(
        &self,
        session_id: i32,
        project_id: i32,
        environment_id: Option<i32>,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        limit: Option<i32>,
        offset: Option<i32>,
        sort_order: Option<String>,
    ) -> Result<Option<SessionEventsResponse>, AnalyticsError> {
        let request_session = request_sessions::Entity::find()
            .filter(request_sessions::Column::Id.eq(session_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                AnalyticsError::SessionNotFound(format!("Session not found for id: {}", session_id))
            })?;

        let limit_val = limit.unwrap_or(100).min(1000);
        let offset_val = offset.unwrap_or(0);
        let queries = session_events_queries(
            &request_session,
            project_id,
            environment_id,
            (start_date, end_date),
            limit_val,
            offset_val,
            sort_order.as_deref(),
        );

        #[derive(FromQueryResult)]
        struct EventResult {
            event_name: String,
            occurred_at: UtcDateTime,
            event_data: serde_json::Value,
            request_path: String,
            request_query: Option<String>,
        }

        let results = EventResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            queries.page_sql,
            queries.page_values,
        ))
        .all(self.db.as_ref())
        .await?;

        // An empty page reports a total of 0, as it always has.
        if results.is_empty() {
            return Ok(Some(SessionEventsResponse {
                session_id,
                events: vec![],
                total_count: 0,
                offset: offset_val,
                limit: limit_val,
            }));
        }

        #[derive(FromQueryResult)]
        struct CountResult {
            total_count: i64,
        }

        let total_count = CountResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            queries.count_sql,
            queries.count_values,
        ))
        .one(self.db.as_ref())
        .await?
        .map(|r| r.total_count)
        .unwrap_or(0);

        // `id` is the event's 1-based position in the requested order.
        let events = results
            .into_iter()
            .enumerate()
            .map(|(index, r)| crate::types::responses::SessionEvent {
                id: offset_val
                    .saturating_add(i32::try_from(index).unwrap_or(i32::MAX))
                    .saturating_add(1),
                event_name: r.event_name,
                occurred_at: r.occurred_at,
                event_data: r.event_data,
                request_path: r.request_path,
                request_query: r.request_query,
            })
            .collect();

        Ok(Some(SessionEventsResponse {
            session_id,
            events,
            total_count,
            offset: offset_val,
            limit: limit_val,
        }))
    }

    /// Get session logs
    async fn get_session_logs(
        &self,
        session_id: i32,
        project_id: i32,
        environment_id: Option<i32>,
        visitor_id: Option<i32>,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        limit: Option<i32>,
        offset: Option<i32>,
        sort_order: Option<String>,
    ) -> Result<Option<SessionLogsResponse>, AnalyticsError> {
        let request_session = request_sessions::Entity::find()
            .filter(request_sessions::Column::Id.eq(session_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                AnalyticsError::SessionNotFound(format!("Session not found for id: {}", session_id))
            })?;

        let limit_val = limit.unwrap_or(100).min(1000) as u64;
        let offset_val = offset.unwrap_or(0) as u64;

        // Build query with filters using proxy_logs
        let mut query = temps_entities::proxy_logs::Entity::find()
            .filter(temps_entities::proxy_logs::Column::SessionId.eq(request_session.id))
            .filter(temps_entities::proxy_logs::Column::ProjectId.eq(project_id));

        if let Some(env_id) = environment_id {
            query = query.filter(temps_entities::proxy_logs::Column::EnvironmentId.eq(env_id));
        }

        if let Some(vis_id) = visitor_id {
            query = query.filter(temps_entities::proxy_logs::Column::VisitorId.eq(vis_id));
        }

        if let Some(start) = start_date {
            query = query.filter(temps_entities::proxy_logs::Column::Timestamp.gte(start));
        }

        if let Some(end) = end_date {
            query = query.filter(temps_entities::proxy_logs::Column::Timestamp.lte(end));
        }

        // Apply ordering
        query = match sort_order.as_deref() {
            Some("asc") => query.order_by_asc(temps_entities::proxy_logs::Column::Timestamp),
            _ => query.order_by_desc(temps_entities::proxy_logs::Column::Timestamp),
        };

        // Get total count
        let total_count = query.clone().count(self.db.as_ref()).await?;

        // Get paginated results using paginator
        let paginator = query.paginate(self.db.as_ref(), limit_val);
        let page_number = offset_val / limit_val;
        let results = paginator.fetch_page(page_number).await?;

        if results.is_empty() {
            return Ok(Some(SessionLogsResponse {
                session_id,
                logs: vec![],
                total_count: 0,
                offset: offset_val as i32,
                limit: limit_val as i32,
            }));
        }

        let logs = results
            .into_iter()
            .map(|r| crate::types::responses::SessionRequestLog {
                id: r.id,
                method: r.method,
                path: r.path,
                status_code: r.status_code,
                response_time_ms: r.response_time_ms,
                created_at: r.timestamp,
                user_agent: r.user_agent,
                referrer: r.referrer,
                response_headers: r
                    .response_headers
                    .and_then(|v| serde_json::to_string(&v).ok()),
                request_headers: r
                    .request_headers
                    .and_then(|v| serde_json::to_string(&v).ok()),
            })
            .collect();

        Ok(Some(SessionLogsResponse {
            session_id,
            logs,
            total_count: total_count as i64,
            offset: offset_val as i32,
            limit: limit_val as i32,
        }))
    }

    /// Enrich visitor by ID
    async fn enrich_visitor_by_id(
        &self,
        visitor_id: i32,
        project_id: Option<i32>,
        enrichment_data: serde_json::Value,
    ) -> Result<EnrichVisitorResponse, AnalyticsError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::visitor;

        let mut query = visitor::Entity::find().filter(visitor::Column::Id.eq(visitor_id));
        if let Some(project_id) = project_id {
            query = query.filter(visitor::Column::ProjectId.eq(project_id));
        }
        let Some(visitor_model) = query
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?
        else {
            return Ok(visitor_not_found(visitor_id.to_string()));
        };

        self.apply_visitor_enrichment(
            visitor_model,
            project_id.is_some(),
            enrichment_data,
            visitor_id.to_string(),
        )
        .await
    }

    /// Enrich visitor by GUID (visitor_id string, may be encrypted with enc_ prefix)
    async fn enrich_visitor_by_guid(
        &self,
        visitor_guid: &str,
        project_id: Option<i32>,
        enrichment_data: serde_json::Value,
    ) -> Result<EnrichVisitorResponse, AnalyticsError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::visitor;

        // Handle encrypted visitor ID (enc_ prefix)
        let actual_visitor_id = if let Some(encrypted) = visitor_guid.strip_prefix("enc_") {
            match self.cookie_crypto.decrypt(encrypted) {
                Ok(decrypted) => decrypted,
                Err(_) => {
                    // A project-scoped caller must not be able to tell an
                    // undecryptable value from an unknown visitor: that would
                    // let it probe the shared cookie key.
                    if project_id.is_some() {
                        return Ok(visitor_not_found(visitor_guid.to_string()));
                    }
                    return Err(AnalyticsError::InvalidVisitorId(visitor_guid.to_string()));
                }
            }
        } else {
            visitor_guid.to_string()
        };

        // Find the visitor by visitor_id (guid), confined to the caller's project
        // when one is given so a project-scoped credential cannot reach another
        // tenant's visitors.
        let mut query =
            visitor::Entity::find().filter(visitor::Column::VisitorId.eq(&actual_visitor_id));
        if let Some(project_id) = project_id {
            query = query.filter(visitor::Column::ProjectId.eq(project_id));
        }
        let Some(visitor_model) = query
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?
        else {
            // Echo the caller's own input, never the decrypted plaintext: the
            // cookie key is shared with auth cookies, so returning it would make
            // this endpoint a decryption oracle.
            return Ok(visitor_not_found(visitor_guid.to_string()));
        };

        self.apply_visitor_enrichment(
            visitor_model,
            project_id.is_some(),
            enrichment_data,
            visitor_guid.to_string(),
        )
        .await
    }

    /// Check if analytics events exist
    async fn has_analytics_events(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<crate::types::responses::HasAnalyticsEventsResponse, AnalyticsError> {
        let mut query = events::Entity::find().filter(events::Column::ProjectId.eq(project_id));

        if let Some(env_id) = environment_id {
            query = query.filter(events::Column::EnvironmentId.eq(env_id));
        }

        let count = query.count(self.db.as_ref()).await?;

        Ok(crate::types::responses::HasAnalyticsEventsResponse {
            has_events: count > 0,
        })
    }

    /// Get all unique page paths for a project with time on page metrics
    async fn get_page_paths(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        limit: Option<i32>,
    ) -> Result<crate::types::responses::PagePathsResponse, AnalyticsError> {
        // Build WHERE conditions with parameterized queries
        let mut where_conditions = vec![
            "pv.project_id = $1".to_string(),
            "pv.page_path IS NOT NULL".to_string(),
            "pv.page_path != ''".to_string(),
            "pv.event_type = 'page_view'".to_string(),
            // Crawler page views are excluded here for the same reason they are
            // excluded from the headline unique counts: a per-page table that
            // counts bots cannot be reconciled against a total that doesn't.
            "pv.is_crawler = false".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let mut param_index = 2;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("pv.environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        // Use provided dates or default to last 24 hours
        let (start, end) = if let (Some(start), Some(end)) = (start_date, end_date) {
            (start, end)
        } else if let Some(start) = start_date {
            (start, start + chrono::Duration::days(1))
        } else {
            // Default to last 24 hours
            let end = chrono::Utc::now();
            let start = end - chrono::Duration::days(1);
            (start, end)
        };

        where_conditions.push(format!("pv.timestamp >= ${}", param_index));
        values.push(start.into());
        param_index += 1;

        where_conditions.push(format!("pv.timestamp <= ${}", param_index));
        values.push(end.into());
        param_index += 1;

        let limit_val = limit.unwrap_or(100).min(1000);
        let where_clause = where_conditions.join(" AND ");

        // Materialize session events in a CTE, then find each page_view's next
        // events with window functions over the session ordered by time,
        // preserving the exact 3-priority time_on_page logic:
        //   Priority 1: next page_leave for same session+page within 30 min
        //   Priority 2: next page_view in same session (any page)
        //   Priority 3: fallback 30 seconds
        // Window functions are O(N log N) in a session's events; self-joins
        // of the CTE (which has no indexes) were O(N^2) per session.
        let sql_query = format!(
            r#"
            WITH session_events AS MATERIALIZED (
                -- Materialize all page_view and page_leave events for matching sessions
                SELECT
                    e.id as event_id,
                    e.session_id,
                    e.event_type,
                    e.page_path,
                    e.timestamp
                FROM events e
                WHERE e.session_id IN (
                    SELECT DISTINCT pv.session_id
                    FROM events pv
                    WHERE {where_clause}
                )
                AND e.event_type IN ('page_view', 'page_leave')
                -- Also filter here, not only in the session sub-select above:
                -- qualifying a session on one human event would otherwise drag
                -- in every crawler-flagged row that shares its session id.
                AND e.is_crawler = false
            ),
            -- Earliest page_leave on the same page and earliest page_view
            -- strictly after each event: a running MIN over the rows already
            -- seen in descending time order, excluding same-timestamp peers.
            -- The frame head stays at the partition start, so PostgreSQL
            -- accumulates each in a single pass.
            next_events AS (
                SELECT
                    se.*,
                    MIN(se.timestamp) FILTER (WHERE se.event_type = 'page_leave') OVER (
                        PARTITION BY se.session_id, se.page_path
                        ORDER BY se.timestamp DESC
                        RANGE BETWEEN UNBOUNDED PRECEDING AND '1 microsecond' PRECEDING
                    ) as next_page_leave_any_ts,
                    MIN(se.timestamp) FILTER (WHERE se.event_type = 'page_view') OVER (
                        PARTITION BY se.session_id
                        ORDER BY se.timestamp DESC
                        RANGE BETWEEN UNBOUNDED PRECEDING AND '1 microsecond' PRECEDING
                    ) as next_page_view_ts
                FROM session_events se
            ),
            page_durations AS (
                SELECT
                    ne.page_path,
                    ne.session_id,
                    ne.timestamp as first_seen_ts,
                    EXTRACT(EPOCH FROM (
                        COALESCE(
                            -- Priority 1 only counts within 30 min.
                            CASE
                                WHEN ne.next_page_leave_any_ts
                                    <= ne.timestamp + INTERVAL '30 minutes'
                                THEN ne.next_page_leave_any_ts
                            END,
                            ne.next_page_view_ts,
                            ne.timestamp + INTERVAL '30 seconds'
                        ) - ne.timestamp
                    )) as time_on_page_seconds
                FROM next_events ne
                WHERE ne.event_type = 'page_view'
                  AND ne.page_path IS NOT NULL
                  AND ne.page_path != ''
            )
            SELECT
                page_path,
                COUNT(DISTINCT session_id) as session_count,
                COUNT(*) as page_view_count,
                ROUND(AVG(
                    CASE
                        WHEN time_on_page_seconds > 0 AND time_on_page_seconds < 1800
                        THEN time_on_page_seconds
                    END
                )::numeric, 1)::float8 as avg_time_seconds,
                MIN(first_seen_ts) as first_seen,
                MAX(first_seen_ts) as last_seen
            FROM page_durations
            GROUP BY page_path
            ORDER BY page_view_count DESC
            LIMIT ${param_index}
            "#,
            where_clause = where_clause,
            param_index = param_index
        );

        // Add LIMIT as parameter
        values.push((limit_val as i64).into());

        #[derive(FromQueryResult)]
        struct PagePathResult {
            page_path: String,
            session_count: i64,
            page_view_count: i64,
            avg_time_seconds: Option<f64>,
            first_seen: UtcDateTime,
            last_seen: UtcDateTime,
        }

        let results = PagePathResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        let page_paths: Vec<crate::types::responses::PagePathInfo> = results
            .into_iter()
            .map(|r| crate::types::responses::PagePathInfo {
                page_path: r.page_path,
                session_count: r.session_count,
                page_view_count: r.page_view_count,
                avg_time_seconds: r.avg_time_seconds,
                first_seen: r.first_seen,
                last_seen: r.last_seen,
            })
            .collect();

        let total_count = page_paths.len();
        Ok(crate::types::responses::PagePathsResponse {
            page_paths,
            total_count,
        })
    }

    /// Get the count of active visitors in real-time
    /// Active visitors are defined as unique sessions with events in the last 5 minutes
    async fn get_active_visitors_count(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        minutes: Option<i32>,
    ) -> Result<i64, AnalyticsError> {
        let window = minutes.unwrap_or(5);
        // Define active window as last 5 minutes
        let query = r#"SELECT COUNT(DISTINCT session_id) as active_visitors
FROM events
WHERE project_id = $1
  AND ($2::int IS NULL OR environment_id = $2)
  AND ($3::int IS NULL OR deployment_id = $3)
  AND timestamp >= NOW() - INTERVAL '5 minutes'"#;

        #[derive(FromQueryResult)]
        struct ActiveVisitorsResult {
            active_visitors: i64,
        }

        let params = vec![project_id.into(), environment_id.into(), window.into()];

        let result = ActiveVisitorsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            query,
            params,
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(ActiveVisitorsResult { active_visitors: 0 });

        Ok(result.active_visitors)
    }

    /// Get real-time active visitors with session details
    /// Returns sessions with activity in the last N minutes
    async fn get_active_visitors_details(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        minutes: Option<i32>,
        limit: Option<i32>,
    ) -> Result<crate::types::responses::ActiveVisitorsResponse, AnalyticsError> {
        let window = minutes.unwrap_or(5);

        // If limit is provided, add LIMIT clause to the query
        let query = if let Some(limit) = limit {
            format!(
                r#"
                SELECT
                    e.session_id,
                    e.visitor_id,
                    MIN(e.timestamp) as session_start,
                    MAX(e.timestamp) as last_activity,
                    COUNT(DISTINCT e.page_path) as page_count,
                    COUNT(*) as event_count,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp DESC))[1] as current_page,
                    EXTRACT(EPOCH FROM (MAX(e.timestamp) - MIN(e.timestamp)))::DOUBLE PRECISION as duration_seconds
                FROM events e
                WHERE e.project_id = $1
                  AND ($2::int IS NULL OR e.environment_id = $2)
                  AND ($3::int IS NULL OR e.deployment_id = $3)
                  AND e.timestamp >= NOW() - INTERVAL '{} minutes'
                GROUP BY e.session_id, e.visitor_id
                ORDER BY last_activity DESC
                LIMIT {}
                "#,
                window, limit
            )
        } else {
            format!(
                r#"
                SELECT
                    e.session_id,
                    e.visitor_id,
                    MIN(e.timestamp) as session_start,
                    MAX(e.timestamp) as last_activity,
                    COUNT(DISTINCT e.page_path) as page_count,
                    COUNT(*) as event_count,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp DESC))[1] as current_page,
                    EXTRACT(EPOCH FROM (MAX(e.timestamp) - MIN(e.timestamp)))::DOUBLE PRECISION as duration_seconds
                FROM events e
                WHERE e.project_id = $1
                  AND ($2::int IS NULL OR e.environment_id = $2)
                  AND ($3::int IS NULL OR e.deployment_id = $3)
                  AND e.timestamp >= NOW() - INTERVAL '{} minutes'
                GROUP BY e.session_id, e.visitor_id
                ORDER BY last_activity DESC
                "#,
                window
            )
        };

        #[derive(FromQueryResult)]
        struct ActiveVisitorData {
            session_id: Option<String>,
            visitor_id: Option<i32>,
            session_start: UtcDateTime,
            last_activity: UtcDateTime,
            page_count: i64,
            event_count: i64,
            current_page: Option<String>,
            duration_seconds: Option<f64>,
        }

        let params = vec![project_id.into(), environment_id.into(), window.into()];

        let results = ActiveVisitorData::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &query,
            params,
        ))
        .all(self.db.as_ref())
        .await?;

        let active_visitors: Vec<crate::types::responses::ActiveVisitor> = results
            .into_iter()
            .filter_map(|data| {
                data.session_id.map(|session_id| {
                    crate::types::responses::ActiveVisitor {
                        session_id,
                        visitor_id: data.visitor_id.map(|id| id.to_string()),
                        session_start: data.session_start,
                        last_activity: data.last_activity,
                        page_count: data.page_count as i32,
                        event_count: data.event_count as i32,
                        current_page: data.current_page,
                        duration_seconds: data.duration_seconds.unwrap_or(0.0) as i64,
                        is_active: true, // Always true since we're querying recent activity
                    }
                })
            })
            .collect();

        let count = active_visitors.len() as i64;
        Ok(crate::types::responses::ActiveVisitorsResponse {
            visitors: active_visitors,
            count,
            window_minutes: window,
        })
    }

    /// Get sparkline data for multiple page paths in a single query.
    /// This avoids the N+1 problem where each page path in the list view
    /// would otherwise trigger its own query with ~150ms planning overhead.
    async fn get_page_paths_sparklines(
        &self,
        project_id: i32,
        page_paths: &[String],
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
    ) -> Result<crate::types::responses::PagePathsSparklineResponse, AnalyticsError> {
        if page_paths.is_empty() {
            return Ok(crate::types::responses::PagePathsSparklineResponse { sparklines: vec![] });
        }

        // Build page_path placeholders: $5, $6, ... (first 4 params are start, end, project, env)
        let mut values: Vec<sea_orm::Value> =
            vec![start_date.into(), end_date.into(), project_id.into()];
        let mut param_index = 4;

        let env_filter = if let Some(env_id) = environment_id {
            values.push(env_id.into());
            let filter = format!("AND e.environment_id = ${}", param_index);
            param_index += 1;
            filter
        } else {
            String::new()
        };

        let path_placeholders: Vec<String> = page_paths
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let placeholder = format!("${}", param_index + i);
                placeholder
            })
            .collect();
        let in_clause = path_placeholders.join(", ");

        for path in page_paths {
            values.push(path.clone().into());
        }

        // Determine bucket interval based on date range
        let duration = end_date - start_date;
        let (interval_str, date_trunc_unit) = if duration.num_days() <= 2 {
            ("1 hour", "hour")
        } else if duration.num_days() <= 31 {
            ("1 day", "day")
        } else if duration.num_days() <= 180 {
            ("1 week", "week")
        } else {
            ("1 month", "month")
        };

        // Single query for all page paths: one planning pass instead of N
        let sql = format!(
            r#"
            WITH time_buckets AS (
                SELECT generate_series(
                    date_trunc('{date_trunc}', $1::timestamp),
                    date_trunc('{date_trunc}', $2::timestamp),
                    '{interval}'::interval
                ) AS time_bucket
            ),
            session_stats AS (
                SELECT
                    e.page_path,
                    date_trunc('{date_trunc}', e.timestamp) as time_bucket,
                    COUNT(DISTINCT e.session_id) as session_count
                FROM events e
                WHERE e.project_id = $3
                    AND e.page_path IN ({in_clause})
                    AND e.event_type = 'page_view'
                    AND e.timestamp >= $1::timestamp
                    AND e.timestamp <= $2::timestamp
                    AND e.session_id IS NOT NULL
                    {env_filter}
                GROUP BY e.page_path, date_trunc('{date_trunc}', e.timestamp)
            )
            SELECT
                ss.page_path,
                to_char(tb.time_bucket, 'YYYY-MM-DD HH24:MI:SS') as timestamp,
                COALESCE(ss.session_count, 0) as session_count
            FROM time_buckets tb
            LEFT JOIN session_stats ss ON tb.time_bucket = ss.time_bucket
            WHERE ss.page_path IS NOT NULL
            ORDER BY ss.page_path, tb.time_bucket
            "#,
            date_trunc = date_trunc_unit,
            interval = interval_str,
            in_clause = in_clause,
            env_filter = env_filter,
        );

        #[derive(FromQueryResult)]
        struct SparklineRow {
            page_path: String,
            timestamp: String,
            session_count: i64,
        }

        let rows = SparklineRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        // Group by page_path
        let mut sparkline_map: std::collections::HashMap<
            String,
            Vec<crate::types::responses::PagePathSparklinePoint>,
        > = std::collections::HashMap::new();

        for row in rows {
            sparkline_map
                .entry(row.page_path.clone())
                .or_default()
                .push(crate::types::responses::PagePathSparklinePoint {
                    timestamp: row.timestamp,
                    session_count: row.session_count,
                });
        }

        // Preserve the order of page_paths from the request
        let sparklines = page_paths
            .iter()
            .map(|path| crate::types::responses::PagePathSparkline {
                page_path: path.clone(),
                points: sparkline_map.remove(path.as_str()).unwrap_or_default(),
            })
            .collect();

        Ok(crate::types::responses::PagePathsSparklineResponse { sparklines })
    }

    /// Get hourly session statistics for a specific page
    async fn get_page_hourly_sessions(
        &self,
        project_id: i32,
        page_path: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
    ) -> Result<crate::types::responses::PageHourlySessionsResponse, AnalyticsError> {
        // Default to hourly intervals
        let bucket_interval = "hour";
        let (interval_str, date_trunc_unit) = match bucket_interval {
            "hour" => ("1 hour", "hour"),
            "day" => ("1 day", "day"),
            "week" => ("1 week", "week"),
            "month" => ("1 month", "month"),
            _ => ("1 hour", "hour"), // Default to hour
        };

        // Query using generate_series for proper gap filling
        let query = format!(
            r#"
            WITH time_buckets AS (
                SELECT generate_series(
                    date_trunc('{}', $1::timestamp),
                    date_trunc('{}', $2::timestamp),
                    '{}'::interval
                ) AS time_bucket
            ),
            session_stats AS (
                SELECT
                    date_trunc('{}', timestamp) as time_bucket,
                    COUNT(DISTINCT session_id) as session_count,
                    COUNT(*) as event_count,
                    AVG(time_on_page::float) as avg_time_on_page
                FROM events
                WHERE project_id = $3
                    AND page_path = $4
                    AND timestamp >= $1::timestamp
                    AND timestamp <= $2::timestamp
                    AND session_id IS NOT NULL
                    {}
                GROUP BY date_trunc('{}', timestamp)
            )
            SELECT
                to_char(tb.time_bucket, 'YYYY-MM-DD HH24:MI:SS') as timestamp,
                COALESCE(ss.session_count, 0) as session_count,
                COALESCE(ss.event_count, 0) as event_count,
                COALESCE(ss.avg_time_on_page, 0) as avg_duration_seconds
            FROM time_buckets tb
            LEFT JOIN session_stats ss ON tb.time_bucket = ss.time_bucket
            ORDER BY tb.time_bucket
            "#,
            date_trunc_unit,
            date_trunc_unit,
            interval_str,
            date_trunc_unit,
            environment_id.map_or(String::new(), |id| format!("AND environment_id = {}", id)),
            date_trunc_unit
        );

        #[derive(FromQueryResult)]
        struct HourlyPageData {
            timestamp: String,
            session_count: i64,
            event_count: i64,
            avg_duration_seconds: f64,
        }

        let results = HourlyPageData::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &query,
            vec![
                start_date.into(),
                end_date.into(),
                project_id.into(),
                page_path.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let hourly_sessions: Vec<crate::types::responses::HourlyPageSessions> = results
            .into_iter()
            .map(|data| super::types::responses::HourlyPageSessions {
                timestamp: data.timestamp,
                session_count: data.session_count,
                event_count: data.event_count,
                avg_duration_seconds: data.avg_duration_seconds,
            })
            .collect();

        let total_sessions = hourly_sessions.iter().map(|h| h.session_count).sum();
        let hours = hourly_sessions.len() as i32;
        let page_path_str = page_path.to_string();
        Ok(crate::types::responses::PageHourlySessionsResponse {
            hourly_data: hourly_sessions,
            total_sessions,
            hours,
            page_path: page_path_str,
        })
    }

    async fn get_visitor_with_geolocation_by_id(
        &self,
        id: i32,
    ) -> Result<Option<crate::types::responses::VisitorWithGeolocation>, AnalyticsError> {
        use sea_orm::{EntityTrait, JoinType, QuerySelect, RelationTrait};
        use temps_entities::{ip_geolocations, visitor};

        let query = visitor::Entity::find_by_id(id)
            .join(JoinType::LeftJoin, visitor::Relation::IpGeolocations.def());

        let result = query
            .select_also(ip_geolocations::Entity)
            .one(self.db.as_ref())
            .await?;

        match result {
            Some((visitor_model, geo_opt)) => {
                let response = crate::types::responses::VisitorWithGeolocation {
                    id: visitor_model.id,
                    visitor_id: visitor_model.visitor_id,
                    project_id: visitor_model.project_id,
                    environment_id: visitor_model.environment_id,
                    first_seen: visitor_model.first_seen,
                    last_seen: visitor_model.last_seen,
                    user_agent: visitor_model.user_agent,
                    is_crawler: visitor_model.is_crawler,
                    crawler_name: visitor_model.crawler_name,
                    custom_data: visitor_model.custom_data,
                    // Geolocation fields
                    ip_address: geo_opt.as_ref().map(|g| g.ip_address.clone()),
                    latitude: geo_opt.as_ref().and_then(|g| g.latitude),
                    longitude: geo_opt.as_ref().and_then(|g| g.longitude),
                    region: geo_opt.as_ref().and_then(|g| g.region.clone()),
                    city: geo_opt.as_ref().and_then(|g| g.city.clone()),
                    country: geo_opt.as_ref().map(|g| g.country.clone()),
                    country_code: geo_opt.as_ref().and_then(|g| g.country_code.clone()),
                    timezone: geo_opt.as_ref().and_then(|g| g.timezone.clone()),
                    is_eu: geo_opt.as_ref().map(|g| g.is_eu),
                    first_referrer: visitor_model.first_referrer,
                    first_referrer_hostname: visitor_model.first_referrer_hostname,
                    first_channel: visitor_model.first_channel,
                };
                Ok(Some(response))
            }
            None => Ok(None),
        }
    }

    async fn get_visitor_with_geolocation_by_guid(
        &self,
        visitor_id: &str,
    ) -> Result<Option<crate::types::responses::VisitorWithGeolocation>, AnalyticsError> {
        use sea_orm::{EntityTrait, JoinType, QuerySelect, RelationTrait};
        use temps_entities::{ip_geolocations, visitor};

        // Handle encrypted visitor IDs (enc_ prefix)
        let actual_visitor_id = if let Some(encrypted) = visitor_id.strip_prefix("enc_") {
            // Strip the enc_ prefix and decrypt using CookieCrypto
            match self.cookie_crypto.decrypt(encrypted) {
                Ok(decrypted) => decrypted,
                Err(_) => return Err(AnalyticsError::InvalidVisitorId(visitor_id.to_string())),
            }
        } else {
            visitor_id.to_string()
        };

        let query = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(actual_visitor_id))
            .join(JoinType::LeftJoin, visitor::Relation::IpGeolocations.def());

        let result = query
            .select_also(ip_geolocations::Entity)
            .one(self.db.as_ref())
            .await?;

        match result {
            Some((visitor_model, geo_opt)) => {
                let response = crate::types::responses::VisitorWithGeolocation {
                    id: visitor_model.id,
                    visitor_id: visitor_model.visitor_id,
                    project_id: visitor_model.project_id,
                    environment_id: visitor_model.environment_id,
                    first_seen: visitor_model.first_seen,
                    last_seen: visitor_model.last_seen,
                    user_agent: visitor_model.user_agent,
                    is_crawler: visitor_model.is_crawler,
                    crawler_name: visitor_model.crawler_name,
                    custom_data: visitor_model.custom_data,
                    // Geolocation fields
                    ip_address: geo_opt.as_ref().map(|g| g.ip_address.clone()),
                    latitude: geo_opt.as_ref().and_then(|g| g.latitude),
                    longitude: geo_opt.as_ref().and_then(|g| g.longitude),
                    region: geo_opt.as_ref().and_then(|g| g.region.clone()),
                    city: geo_opt.as_ref().and_then(|g| g.city.clone()),
                    country: geo_opt.as_ref().map(|g| g.country.clone()),
                    country_code: geo_opt.as_ref().and_then(|g| g.country_code.clone()),
                    timezone: geo_opt.as_ref().and_then(|g| g.timezone.clone()),
                    is_eu: geo_opt.as_ref().map(|g| g.is_eu),
                    first_referrer: visitor_model.first_referrer,
                    first_referrer_hostname: visitor_model.first_referrer_hostname,
                    first_channel: visitor_model.first_channel,
                };
                Ok(Some(response))
            }
            None => Ok(None),
        }
    }

    async fn get_live_visitors(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        window_minutes: i32,
    ) -> Result<Vec<crate::types::responses::LiveVisitorInfo>, AnalyticsError> {
        let sql = format!(
            r#"
            SELECT
                v.id,
                v.visitor_id,
                v.project_id,
                v.environment_id,
                v.first_seen,
                v.last_seen,
                v.user_agent,
                v.ip_address_id,
                v.is_crawler,
                v.crawler_name,
                v.custom_data,
                ig.ip_address,
                ig.latitude,
                ig.longitude,
                ig.region,
                ig.city,
                ig.country,
                ig.country_code,
                ig.timezone,
                ig.is_eu,
                last_event.page_path as current_page,
                v.first_referrer,
                v.first_referrer_hostname,
                v.first_channel
            FROM visitor v
            LEFT JOIN ip_geolocations ig ON v.ip_address_id = ig.id
            -- Live means a counted event inside the window, as for the active
            -- count: an established visitor stays visible when one of their
            -- visits is excluded, and their `last_seen` keeps moving with it,
            -- so the visitor row alone cannot tell. The page shown is the
            -- latest counted one.
            JOIN LATERAL (
                SELECT e.page_path
                FROM events e
                WHERE e.visitor_id = v.id
                  AND e.timestamp >= NOW() - INTERVAL '1 minute' * $3
                  AND e.is_crawler = false
                ORDER BY e.timestamp DESC
                LIMIT 1
            ) last_event ON true
            WHERE v.project_id = $1
              AND ($2::int IS NULL OR v.environment_id = $2)
              AND v.last_seen >= NOW() - INTERVAL '1 minute' * $3
              AND v.is_crawler = false
              AND {hosting_counted}
            ORDER BY v.last_seen DESC
        "#,
            hosting_counted = crate::datacenter::hosting_traffic_counted("ig", "$1"),
        );

        #[derive(FromQueryResult)]
        struct LiveVisitorRow {
            id: i32,
            visitor_id: String,
            project_id: i32,
            environment_id: i32,
            first_seen: UtcDateTime,
            last_seen: UtcDateTime,
            user_agent: Option<String>,
            ip_address_id: Option<i32>,
            is_crawler: bool,
            crawler_name: Option<String>,
            custom_data: Option<serde_json::Value>,
            ip_address: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
            region: Option<String>,
            city: Option<String>,
            country: Option<String>,
            country_code: Option<String>,
            timezone: Option<String>,
            is_eu: Option<bool>,
            current_page: Option<String>,
            first_referrer: Option<String>,
            first_referrer_hostname: Option<String>,
            first_channel: Option<String>,
        }

        let rows = LiveVisitorRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                project_id.into(),
                environment_id.into(),
                window_minutes.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let visitors = rows
            .into_iter()
            .map(|row| crate::types::responses::LiveVisitorInfo {
                id: row.id,
                visitor_id: row.visitor_id,
                project_id: row.project_id,
                environment_id: row.environment_id,
                first_seen: row.first_seen,
                last_seen: row.last_seen,
                user_agent: row.user_agent,
                ip_address_id: row.ip_address_id,
                is_crawler: row.is_crawler,
                crawler_name: row.crawler_name,
                custom_data: row.custom_data,
                ip_address: row.ip_address,
                latitude: row.latitude,
                longitude: row.longitude,
                region: row.region,
                city: row.city,
                country: row.country,
                country_code: row.country_code,
                timezone: row.timezone,
                is_eu: row.is_eu,
                current_page: row.current_page,
                first_referrer: row.first_referrer,
                first_referrer_hostname: row.first_referrer_hostname,
                first_channel: row.first_channel,
            })
            .collect();

        Ok(visitors)
    }

    async fn get_general_stats(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
    ) -> Result<crate::types::responses::GeneralStatsResponse, AnalyticsError> {
        // Query to get overall stats across all projects. Event totals and
        // session spans leave out traffic-filter exclusions only: they never
        // left bots out, and server-side events are stored as the bot
        // `unknown`.
        let not_filtered = temps_entities::events::not_excluded_by_traffic_filter("e");
        let not_filtered_bare = temps_entities::events::not_excluded_by_traffic_filter("");
        let total_stats_sql = format!(
            r#"
            -- Optimized: avoids join fan-out, uses half-open (>= $1 AND < $2) intervals
            WITH
                unique_visitors AS (
                    SELECT COUNT(DISTINCT e.visitor_id) AS n
                    FROM events e
                    WHERE e.event_type = 'page_view'
                      AND e.is_crawler = false
                      AND e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_visits AS (
                    -- Sessions shown in analytics must have a real browser
                    -- page view. Proxy-only request sessions (for example API
                    -- traffic created by older versions) are intentionally
                    -- excluded so historical polluted rows cannot inflate the
                    -- dashboard after the proxy-side fix is deployed.
                    SELECT COUNT(DISTINCT CASE
                        WHEN e.session_id LIKE 'v2|%'
                            THEN split_part(e.session_id, '|', 2)
                        ELSE e.session_id
                    END) AS n
                    FROM events e
                    WHERE e.event_type = 'page_view'
                      AND e.is_crawler = false
                      AND e.session_id IS NOT NULL
                      AND e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_events AS (
                    SELECT COUNT(*) AS n
                    FROM events e
                    WHERE {not_filtered}
                      AND e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_page_views AS (
                    SELECT COUNT(*) AS n
                    FROM events e
                    WHERE e.event_type = 'page_view'
                      AND e.is_crawler = false
                      AND e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_projects AS (
                    SELECT COUNT(*) AS n
                    FROM projects p
                ),
                -- Bounce and engagement are derived per session at query time:
                -- a bounce is a session with at most one page view; an engaged
                -- session lasted >= 10s or fired a genuine interaction event
                -- (auto-fired *_viewed events excluded — they trigger from
                -- intersection observers for bots too). The events.is_bounce
                -- column is never populated by the ingest path.
                session_stats AS (
                    SELECT
                        CASE
                            WHEN session_id LIKE 'v2|%'
                                THEN split_part(session_id, '|', 2)
                            ELSE session_id
                        END AS session_id,
                        COUNT(*) FILTER (
                            WHERE event_type = 'page_view' AND is_crawler = false
                        ) AS page_views,
                        COUNT(*) FILTER (
                            WHERE event_type NOT IN ('page_view', 'page_leave')
                              AND event_type NOT LIKE '%\_viewed' ESCAPE '\'
                              AND is_crawler = false
                        ) AS interaction_events,
                        EXTRACT(EPOCH FROM (MAX(timestamp) - MIN(timestamp))) AS duration_seconds
                    FROM events
                    -- Excluded rows would otherwise stretch a session's
                    -- duration, or count a bot-only session as a bounce.
                    WHERE {not_filtered_bare}
                      AND timestamp >= $1 AND timestamp < $2
                      AND session_id IS NOT NULL
                    GROUP BY CASE
                        WHEN session_id LIKE 'v2|%'
                            THEN split_part(session_id, '|', 2)
                        ELSE session_id
                    END
                    HAVING COUNT(*) FILTER (
                        WHERE event_type = 'page_view' AND is_crawler = false
                    ) > 0
                ),
                session_rates AS (
                    SELECT
                        COALESCE((COUNT(*) FILTER (WHERE page_views <= 1))::float
                                 / NULLIF(COUNT(*), 0)::float * 100, 0) AS avg_bounce_rate,
                        COALESCE((COUNT(*) FILTER (WHERE duration_seconds >= 10
                                                      OR interaction_events > 0))::float
                                 / NULLIF(COUNT(*), 0)::float * 100, 0) AS avg_engagement_rate
                    FROM session_stats
                )
            SELECT
                unique_visitors.n AS unique_visitors,
                total_visits.n AS total_visits,
                total_page_views.n AS total_page_views,
                total_events.n AS total_events,
                total_projects.n AS total_projects,
                session_rates.avg_bounce_rate::double precision AS avg_bounce_rate,
                session_rates.avg_engagement_rate::double precision AS avg_engagement_rate
            FROM unique_visitors, total_visits, total_page_views, total_events, total_projects, session_rates
        "#
        );

        #[derive(FromQueryResult)]
        struct TotalStatsResult {
            unique_visitors: i64,
            total_visits: i64,
            total_page_views: i64,
            total_events: i64,
            total_projects: i64,
            avg_bounce_rate: f64,
            avg_engagement_rate: f64,
        }

        let total_stats = TotalStatsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            total_stats_sql,
            vec![start_date.into(), end_date.into()],
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(TotalStatsResult {
            unique_visitors: 0,
            total_visits: 0,
            total_page_views: 0,
            total_events: 0,
            total_projects: 0,
            avg_bounce_rate: 0.0,
            avg_engagement_rate: 0.0,
        });

        // Query previous period for trend comparison (same duration, shifted back)
        let period_duration = end_date - start_date;
        let prev_start = start_date - period_duration;
        let prev_end = start_date;

        // Lightweight previous-period query: only visitors and page views
        // Uses events_hourly continuous aggregate for speed
        let prev_stats_sql = r#"
            SELECT
                COALESCE(SUM(unique_visitors), 0)::bigint AS prev_visitors,
                COALESCE(SUM(page_views), 0)::bigint AS prev_page_views
            FROM events_hourly
            WHERE bucket >= $1 AND bucket < $2
        "#;

        #[derive(FromQueryResult)]
        struct PrevStatsResult {
            prev_visitors: i64,
            prev_page_views: i64,
        }

        let prev_stats = PrevStatsResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            prev_stats_sql,
            vec![prev_start.into(), prev_end.into()],
        ))
        .one(self.db.as_ref())
        .await?;

        let (previous_unique_visitors, previous_page_views, visitors_trend, page_views_trend) =
            if let Some(prev) = prev_stats {
                let visitors_trend = if prev.prev_visitors > 0 {
                    Some(
                        ((total_stats.unique_visitors - prev.prev_visitors) as f64
                            / prev.prev_visitors as f64)
                            * 100.0,
                    )
                } else if total_stats.unique_visitors > 0 {
                    Some(100.0)
                } else {
                    None
                };

                let pv_trend = if prev.prev_page_views > 0 {
                    Some(
                        ((total_stats.total_page_views - prev.prev_page_views) as f64
                            / prev.prev_page_views as f64)
                            * 100.0,
                    )
                } else if total_stats.total_page_views > 0 {
                    Some(100.0)
                } else {
                    None
                };

                (
                    Some(prev.prev_visitors),
                    Some(prev.prev_page_views),
                    visitors_trend,
                    pv_trend,
                )
            } else {
                (None, None, None, None)
            };

        Ok(crate::types::responses::GeneralStatsResponse {
            total_unique_visitors: total_stats.unique_visitors,
            total_visits: total_stats.total_visits,
            total_page_views: total_stats.total_page_views,
            total_events: total_stats.total_events,
            total_projects: total_stats.total_projects,
            avg_bounce_rate: total_stats.avg_bounce_rate,
            avg_engagement_rate: total_stats.avg_engagement_rate,
            previous_unique_visitors,
            previous_page_views,
            visitors_trend_percentage: visitors_trend,
            page_views_trend_percentage: page_views_trend,
            project_breakdown: Vec::new(),
        })
    }

    /// Get individual visitor sessions for a specific page path
    async fn get_page_path_visitors(
        &self,
        project_id: i32,
        page_path: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<crate::types::responses::PagePathVisitorsResponse, AnalyticsError> {
        let per_page = per_page.min(100);
        let offset = page.saturating_sub(1).saturating_mul(per_page);
        let limit_value = i64::try_from(per_page).unwrap_or(i64::MAX);
        let offset_value = i64::try_from(offset).unwrap_or(i64::MAX);

        // Count total matching rows
        let count_query = r#"
            SELECT COUNT(*) as count
            FROM events e
            WHERE e.project_id = $1
              AND e.page_path = $2
              AND e.event_type = 'page_view'
              AND e.timestamp >= $3
              AND e.timestamp <= $4
              AND e.is_crawler = false
              AND ($5::int IS NULL OR e.environment_id = $5)
            "#;

        let count_stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            count_query,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
            ],
        );

        #[derive(Debug, sea_orm::FromQueryResult)]
        struct CountRow {
            count: i64,
        }

        let total_count = CountRow::find_by_statement(count_stmt)
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::DatabaseError)?
            .map(|r| r.count)
            .unwrap_or(0);

        let (context_start, context_end) = page_path_visitors_context_bounds(start_date, end_date);

        let data_stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            PAGE_PATH_VISITORS_SQL,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
                limit_value.into(),
                offset_value.into(),
                context_start.into(),
                context_end.into(),
                ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
            ],
        );

        #[derive(Debug, sea_orm::FromQueryResult)]
        struct PageVisitorRow {
            visitor_id: Option<i32>,
            visitor_uuid: String,
            session_id: Option<String>,
            viewed_at: UtcDateTime,
            time_on_page: Option<i32>,
            is_entry: bool,
            is_exit: bool,
            is_bounce: bool,
            session_page_number: Option<i32>,
            referrer: Option<String>,
            browser: Option<String>,
            operating_system: Option<String>,
            device_type: Option<String>,
            city: Option<String>,
            country: Option<String>,
            country_code: Option<String>,
        }

        let rows = PageVisitorRow::find_by_statement(data_stmt)
            .all(self.db.as_ref())
            .await
            .map_err(AnalyticsError::DatabaseError)?;

        let sessions = rows
            .into_iter()
            .map(|row| crate::types::responses::PageVisitorSession {
                visitor_id: row.visitor_id.unwrap_or(0),
                visitor_uuid: row.visitor_uuid,
                session_id: row.session_id,
                viewed_at: row.viewed_at,
                time_on_page: row.time_on_page,
                is_entry: row.is_entry,
                is_exit: row.is_exit,
                is_bounce: row.is_bounce,
                session_page_number: row.session_page_number,
                referrer: row.referrer,
                browser: row.browser,
                operating_system: row.operating_system,
                device_type: row.device_type,
                city: row.city,
                country: row.country,
                country_code: row.country_code,
            })
            .collect();

        Ok(crate::types::responses::PagePathVisitorsResponse {
            page_path: page_path.to_string(),
            total_count,
            page,
            per_page,
            sessions,
        })
    }

    async fn get_recent_activity(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        since_id: Option<i64>,
        limit: Option<i32>,
    ) -> Result<crate::types::responses::RecentActivityResponse, AnalyticsError> {
        let limit = std::cmp::min(limit.unwrap_or(50), 100);

        // If since_id is provided, fetch events newer than that ID.
        // Otherwise, fetch the most recent events from the last 60 seconds.
        let sql = r#"
            SELECT
                e.id,
                e.timestamp,
                e.event_type,
                e.event_name,
                e.page_path,
                e.page_title,
                e.visitor_id,
                e.browser,
                e.operating_system,
                e.device_type,
                e.referrer,
                e.is_crawler,
                ig.city,
                ig.country,
                ig.country_code,
                ig.latitude,
                ig.longitude
            FROM events e
            LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
            WHERE e.project_id = $1
              AND ($2::int IS NULL OR e.environment_id = $2)
              AND (
                  ($3::bigint IS NOT NULL AND e.id > $3)
                  OR
                  ($3::bigint IS NULL AND e.timestamp >= NOW() - INTERVAL '60 seconds')
              )
            ORDER BY e.id DESC
            LIMIT $4
        "#;

        #[derive(FromQueryResult)]
        struct ActivityRow {
            id: i64,
            timestamp: UtcDateTime,
            event_type: String,
            event_name: Option<String>,
            page_path: String,
            page_title: Option<String>,
            visitor_id: Option<i32>,
            browser: Option<String>,
            operating_system: Option<String>,
            device_type: Option<String>,
            referrer: Option<String>,
            is_crawler: bool,
            city: Option<String>,
            country: Option<String>,
            country_code: Option<String>,
            latitude: Option<f64>,
            longitude: Option<f64>,
        }

        let rows = ActivityRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                project_id.into(),
                environment_id.into(),
                since_id.into(),
                (limit as i64).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let events = rows
            .into_iter()
            .map(|row| crate::types::responses::ActivityEvent {
                id: row.id,
                timestamp: row.timestamp,
                event_type: row.event_type,
                event_name: row.event_name,
                page_path: row.page_path,
                page_title: row.page_title,
                visitor_id: row.visitor_id,
                browser: row.browser,
                operating_system: row.operating_system,
                device_type: row.device_type,
                referrer: row.referrer,
                city: row.city,
                country: row.country,
                country_code: row.country_code,
                latitude: row.latitude,
                longitude: row.longitude,
                is_crawler: row.is_crawler,
            })
            .collect::<Vec<_>>();

        let count = events.len();

        Ok(crate::types::responses::RecentActivityResponse { events, count })
    }

    /// Get detailed analytics for a specific page path
    async fn get_page_path_detail(
        &self,
        project_id: i32,
        page_path: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        bucket_interval: Option<&str>,
    ) -> Result<crate::types::responses::PagePathDetailResponse, AnalyticsError> {
        // Determine bucket interval based on date range if not specified
        let duration = end_date - start_date;
        let interval = bucket_interval.unwrap_or_else(|| {
            if duration.num_days() <= 1 {
                "hour"
            } else if duration.num_days() <= 31 {
                "day"
            } else if duration.num_days() <= 180 {
                "week"
            } else {
                "month"
            }
        });

        let (pg_interval, date_trunc_unit) = match interval {
            "hour" => ("1 hour", "hour"),
            "day" => ("1 day", "day"),
            "week" => ("1 week", "week"),
            "month" => ("1 month", "month"),
            _ => ("1 day", "day"),
        };

        // 1. Overall page stats (see PAGE_PATH_DETAIL_STATS_SQL).
        let (context_start, context_end) = page_path_visitors_context_bounds(start_date, end_date);

        #[derive(FromQueryResult)]
        struct PageStats {
            unique_visitors: i64,
            total_page_views: i64,
            avg_time_on_page: f64,
            bounce_rate: f64,
            entry_rate: f64,
            exit_rate: f64,
        }

        let stats = PageStats::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            PAGE_PATH_DETAIL_STATS_SQL,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
                context_start.into(),
                context_end.into(),
                ANALYTICS_ROW_CLOCK_SLACK_HOURS.into(),
            ],
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(PageStats {
            unique_visitors: 0,
            total_page_views: 0,
            avg_time_on_page: 0.0,
            bounce_rate: 0.0,
            entry_rate: 0.0,
            exit_rate: 0.0,
        });

        // 2. Get time series data for activity graph
        let activity_sql = format!(
            r#"
            WITH time_buckets AS (
                SELECT generate_series(
                    date_trunc('{}', $3::timestamptz),
                    date_trunc('{}', $4::timestamptz),
                    '{}'::interval
                ) AS bucket
            ),
            page_activity AS (
                SELECT
                    date_trunc('{}', e.timestamp) as bucket,
                    COUNT(DISTINCT e.visitor_id) as visitors,
                    COUNT(*) as page_views,
                    COALESCE(AVG(NULLIF(e.time_on_page, 0)), 0)::float8 as avg_time_seconds
                FROM events e
                WHERE e.project_id = $1
                  AND e.page_path = $2
                  AND e.event_type = 'page_view'
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND ($5::int IS NULL OR e.environment_id = $5)
                GROUP BY date_trunc('{}', e.timestamp)
            )
            SELECT
                tb.bucket::timestamptz as timestamp,
                COALESCE(pa.visitors, 0) as visitors,
                COALESCE(pa.page_views, 0) as page_views,
                COALESCE(pa.avg_time_seconds, 0)::float8 as avg_time_seconds
            FROM time_buckets tb
            LEFT JOIN page_activity pa ON tb.bucket = pa.bucket
            ORDER BY tb.bucket
            "#,
            date_trunc_unit, date_trunc_unit, pg_interval, date_trunc_unit, date_trunc_unit
        );

        #[derive(FromQueryResult)]
        struct ActivityBucket {
            timestamp: UtcDateTime,
            visitors: i64,
            page_views: i64,
            avg_time_seconds: f64,
        }

        let activity_results = ActivityBucket::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &activity_sql,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let activity_over_time: Vec<crate::types::responses::PageActivityBucket> = activity_results
            .into_iter()
            .map(|b| crate::types::responses::PageActivityBucket {
                timestamp: b.timestamp,
                visitors: b.visitors,
                page_views: b.page_views,
                avg_time_seconds: b.avg_time_seconds,
            })
            .collect();

        // 3. Get geographic distribution by country
        let countries_sql = r#"
            WITH country_stats AS (
                SELECT
                    COALESCE(ig.country, 'Unknown') as country,
                    ig.country_code,
                    COUNT(DISTINCT e.visitor_id) as visitors,
                    COUNT(*) as page_views
                FROM events e
                LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
                WHERE e.project_id = $1
                  AND e.page_path = $2
                  AND e.event_type = 'page_view'
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND ($5::int IS NULL OR e.environment_id = $5)
                GROUP BY COALESCE(ig.country, 'Unknown'), ig.country_code
            ),
            total AS (
                SELECT SUM(visitors) as total_visitors FROM country_stats
            )
            SELECT
                cs.country,
                cs.country_code,
                cs.visitors,
                cs.page_views,
                CASE WHEN t.total_visitors > 0
                     THEN cs.visitors::float / t.total_visitors::float * 100
                     ELSE 0 END as percentage
            FROM country_stats cs
            CROSS JOIN total t
            ORDER BY cs.visitors DESC
            LIMIT 50
            "#;

        #[derive(FromQueryResult)]
        struct CountryStats {
            country: String,
            country_code: Option<String>,
            visitors: i64,
            page_views: i64,
            percentage: f64,
        }

        let country_results = CountryStats::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            countries_sql,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let countries: Vec<crate::types::responses::PageCountryStats> = country_results
            .into_iter()
            .map(|c| crate::types::responses::PageCountryStats {
                country: c.country,
                country_code: c.country_code,
                visitors: c.visitors,
                page_views: c.page_views,
                percentage: c.percentage,
            })
            .collect();

        // 4. Get top referrers
        let referrers_sql = r#"
            WITH referrer_stats AS (
                SELECT
                    COALESCE(NULLIF(e.referrer, ''), 'Direct') as referrer,
                    COUNT(*) as visits
                FROM events e
                WHERE e.project_id = $1
                  AND e.page_path = $2
                  AND e.event_type = 'page_view'
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND ($5::int IS NULL OR e.environment_id = $5)
                GROUP BY COALESCE(NULLIF(e.referrer, ''), 'Direct')
            ),
            total AS (
                SELECT SUM(visits) as total_visits FROM referrer_stats
            )
            SELECT
                rs.referrer,
                rs.visits,
                CASE WHEN t.total_visits > 0
                     THEN rs.visits::float / t.total_visits::float * 100
                     ELSE 0 END as percentage
            FROM referrer_stats rs
            CROSS JOIN total t
            ORDER BY rs.visits DESC
            LIMIT 20
            "#;

        #[derive(FromQueryResult)]
        struct ReferrerStats {
            referrer: String,
            visits: i64,
            percentage: f64,
        }

        let referrer_results = ReferrerStats::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            referrers_sql,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
                environment_id.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let referrers: Vec<crate::types::responses::PageReferrerStats> = referrer_results
            .into_iter()
            .map(|r| crate::types::responses::PageReferrerStats {
                referrer: r.referrer,
                visits: r.visits,
                percentage: r.percentage,
            })
            .collect();

        Ok(crate::types::responses::PagePathDetailResponse {
            page_path: page_path.to_string(),
            unique_visitors: stats.unique_visitors,
            total_page_views: stats.total_page_views,
            avg_time_on_page: stats.avg_time_on_page,
            bounce_rate: stats.bounce_rate,
            entry_rate: stats.entry_rate,
            exit_rate: stats.exit_rate,
            activity_over_time,
            countries,
            referrers,
            bucket_interval: interval.to_string(),
        })
    }

    /// Get page flow analytics: entry pages, exit pages, drop-off points, and transitions
    async fn get_page_flow(
        &self,
        project_id: i32,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        limit: Option<i32>,
        transitions_limit: Option<i32>,
        min_views_for_dropoff: Option<i32>,
    ) -> Result<PageFlowResponse, AnalyticsError> {
        let limit = limit.unwrap_or(20).min(100) as i64;
        let transitions_limit = transitions_limit.unwrap_or(50).min(200) as i64;
        let min_views = min_views_for_dropoff.unwrap_or(5) as i64;

        // Build WHERE clause
        let mut where_conditions = vec![
            "e.project_id = $1".to_string(),
            "e.timestamp >= $2".to_string(),
            "e.timestamp <= $3".to_string(),
            "e.event_type = 'page_view'".to_string(),
            "e.is_crawler = false".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> =
            vec![project_id.into(), start_date.into(), end_date.into()];
        let mut param_index = 4;

        if let Some(env_id) = environment_id {
            where_conditions.push(format!("e.environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }

        let where_clause = where_conditions.join(" AND ");

        // Query 1: Page-level stats (entry, exit, bounce, views, avg time).
        // Entry/exit/bounce are session-level facts derived at query time from
        // each session's first/last page view and pageview count — the
        // events.is_entry / is_exit / is_bounce columns are never populated by
        // the ingest path. The window predicate appears twice ({where_clause})
        // but binds the same $n parameters, so both scans ride
        // idx_events_project_timestamp with chunk exclusion.
        let page_stats_sql = format!(
            r#"
            WITH ranked AS (
                -- Window-sorted rather than ARRAY_AGG(...)[1]: ordered-set
                -- aggregates accumulate every row of a session into an array
                -- before taking the first element, so memory grows with total
                -- pageviews in the window. A sort by (session_id, timestamp)
                -- spills to disk as a well-behaved external merge sort and the
                -- downstream GROUP BY streams over the already-sorted input.
                SELECT
                    e.session_id,
                    e.page_path,
                    ROW_NUMBER() OVER w as rn,
                    COUNT(*) OVER (PARTITION BY e.session_id) as pv_total
                FROM events e
                WHERE {where_clause} AND e.session_id IS NOT NULL
                WINDOW w AS (PARTITION BY e.session_id ORDER BY e.timestamp ASC, e.id ASC)
            ),
            sessions AS (
                SELECT
                    session_id,
                    MAX(page_path) FILTER (WHERE rn = 1) as entry_path,
                    MAX(page_path) FILTER (WHERE rn = pv_total) as exit_path,
                    MAX(pv_total) as page_views
                FROM ranked
                GROUP BY session_id
            ),
            entry_stats AS (
                SELECT
                    entry_path as page_path,
                    COUNT(*) as entry_count,
                    COUNT(*) FILTER (WHERE page_views <= 1) as bounce_count
                FROM sessions
                GROUP BY entry_path
            ),
            exit_stats AS (
                SELECT exit_path as page_path, COUNT(*) as exit_count
                FROM sessions
                GROUP BY exit_path
            ),
            page_views_agg AS (
                SELECT
                    e.page_path,
                    COUNT(*) as total_views,
                    (AVG(e.time_on_page) FILTER (WHERE e.time_on_page IS NOT NULL AND e.time_on_page > 0))::float8 as avg_time_on_page
                FROM events e
                WHERE {where_clause}
                GROUP BY e.page_path
            )
            SELECT
                p.page_path,
                p.total_views,
                COALESCE(en.entry_count, 0) as entry_count,
                COALESCE(ex.exit_count, 0) as exit_count,
                COALESCE(en.bounce_count, 0) as bounce_count,
                p.avg_time_on_page
            FROM page_views_agg p
            LEFT JOIN entry_stats en ON en.page_path = p.page_path
            LEFT JOIN exit_stats ex ON ex.page_path = p.page_path
            ORDER BY p.total_views DESC
            "#,
            where_clause = where_clause
        );

        #[derive(FromQueryResult)]
        struct PageStats {
            page_path: String,
            total_views: i64,
            entry_count: i64,
            exit_count: i64,
            bounce_count: i64,
            avg_time_on_page: Option<f64>,
        }

        let page_stats = PageStats::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &page_stats_sql,
            values.clone(),
        ))
        .all(self.db.as_ref())
        .await?;

        let total_pages = page_stats.len() as i64;

        // Build PageFlowEntry list
        let all_entries: Vec<PageFlowEntry> = page_stats
            .iter()
            .map(|p| {
                let entry_rate = if p.total_views > 0 {
                    p.entry_count as f64 / p.total_views as f64
                } else {
                    0.0
                };
                let exit_rate = if p.total_views > 0 {
                    p.exit_count as f64 / p.total_views as f64
                } else {
                    0.0
                };
                let bounce_rate = if p.entry_count > 0 {
                    p.bounce_count as f64 / p.entry_count as f64
                } else {
                    0.0
                };
                PageFlowEntry {
                    page_path: p.page_path.clone(),
                    entry_count: p.entry_count,
                    exit_count: p.exit_count,
                    bounce_count: p.bounce_count,
                    total_views: p.total_views,
                    avg_time_on_page: p.avg_time_on_page,
                    entry_rate,
                    exit_rate,
                    bounce_rate,
                }
            })
            .collect();

        // Top entry pages (sorted by entry_count DESC)
        let mut top_entry_pages = all_entries.clone();
        top_entry_pages.sort_by_key(|b| std::cmp::Reverse(b.entry_count));
        top_entry_pages.truncate(limit as usize);
        // Remove pages with 0 entries
        top_entry_pages.retain(|p| p.entry_count > 0);

        // Top exit pages (sorted by exit_count DESC)
        let mut top_exit_pages = all_entries.clone();
        top_exit_pages.sort_by_key(|b| std::cmp::Reverse(b.exit_count));
        top_exit_pages.truncate(limit as usize);
        top_exit_pages.retain(|p| p.exit_count > 0);

        // Drop-off points: pages with high exit rates and meaningful traffic
        let mut drop_off_points: Vec<DropOffPoint> = all_entries
            .iter()
            .filter(|p| p.total_views >= min_views && p.exit_count > 0)
            .map(|p| DropOffPoint {
                page_path: p.page_path.clone(),
                exit_count: p.exit_count,
                total_views: p.total_views,
                exit_rate: p.exit_rate,
            })
            .collect();
        drop_off_points.sort_by(|a, b| {
            b.exit_rate
                .partial_cmp(&a.exit_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        drop_off_points.truncate(limit as usize);

        // Query 2: Page-to-page transitions using session_page_number
        let transitions_sql = format!(
            r#"
            WITH page_sequence AS (
                SELECT
                    e.session_id,
                    e.page_path,
                    e.session_page_number,
                    LEAD(e.page_path) OVER (
                        PARTITION BY e.session_id
                        ORDER BY COALESCE(e.session_page_number, 0), e.timestamp
                    ) as next_page
                FROM events e
                WHERE {}
            )
            SELECT
                page_path as from_page,
                next_page as to_page,
                COUNT(*) as transition_count
            FROM page_sequence
            WHERE next_page IS NOT NULL
              AND page_path != next_page
            GROUP BY page_path, next_page
            ORDER BY transition_count DESC
            LIMIT ${}
            "#,
            where_clause, param_index
        );

        let mut transition_values = values.clone();
        transition_values.push(transitions_limit.into());

        #[derive(FromQueryResult)]
        struct TransitionResult {
            from_page: String,
            to_page: String,
            transition_count: i64,
        }

        let transition_results =
            TransitionResult::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &transitions_sql,
                transition_values,
            ))
            .all(self.db.as_ref())
            .await?;

        // Calculate percentage of transitions from each source page
        // Build a map of total outgoing transitions per from_page
        let mut from_page_totals: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for t in &transition_results {
            *from_page_totals.entry(t.from_page.clone()).or_insert(0) += t.transition_count;
        }

        let transitions: Vec<PageTransition> = transition_results
            .into_iter()
            .map(|t| {
                let total = from_page_totals.get(&t.from_page).copied().unwrap_or(1);
                PageTransition {
                    from_page: t.from_page,
                    to_page: t.to_page,
                    transition_count: t.transition_count,
                    percentage: (t.transition_count as f64 / total as f64) * 100.0,
                }
            })
            .collect();

        // Query 3: Total sessions count
        let sessions_sql = format!(
            r#"
            SELECT COUNT(DISTINCT e.session_id) as total_sessions
            FROM events e
            WHERE {}
            "#,
            where_clause
        );

        #[derive(FromQueryResult)]
        struct SessionCount {
            total_sessions: i64,
        }

        let session_count = SessionCount::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sessions_sql,
            values,
        ))
        .one(self.db.as_ref())
        .await?;

        let total_sessions = session_count.map(|s| s.total_sessions).unwrap_or(0);

        Ok(PageFlowResponse {
            top_entry_pages,
            top_exit_pages,
            drop_off_points,
            transitions,
            total_pages,
            total_sessions,
        })
    }

    /// Get detailed analytics for a specific event name
    async fn get_event_detail(
        &self,
        project_id: i32,
        event_name: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        bucket_interval: Option<&str>,
    ) -> Result<crate::types::responses::EventDetailResponse, AnalyticsError> {
        // Traffic-filter exclusions only, as in the event counts.
        let not_filtered = temps_entities::events::not_excluded_by_traffic_filter("e");
        // Determine bucket interval based on date range
        let duration = end_date - start_date;
        let interval = bucket_interval.unwrap_or_else(|| {
            if duration.num_days() <= 1 {
                "hour"
            } else if duration.num_days() <= 31 {
                "day"
            } else if duration.num_days() <= 180 {
                "week"
            } else {
                "month"
            }
        });

        let (pg_interval, date_trunc_unit) = match interval {
            "hour" => ("1 hour", "hour"),
            "day" => ("1 day", "day"),
            "week" => ("1 week", "week"),
            "month" => ("1 month", "month"),
            _ => ("1 day", "day"),
        };

        let env_filter = environment_id
            .map(|id| format!("AND e.environment_id = {}", id))
            .unwrap_or_default();

        // 1. Get summary stats
        let stats_sql = format!(
            r#"
            SELECT
                COUNT(*) as total_count,
                COUNT(DISTINCT e.visitor_id) as unique_visitors,
                COUNT(DISTINCT e.session_id) as unique_sessions
            FROM events e
            WHERE e.project_id = $1
              AND COALESCE(e.event_name, e.event_type) = $2
              AND e.timestamp >= $3
              AND e.timestamp < $4
              AND {not_filtered}
              {}
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct EventStats {
            total_count: i64,
            unique_visitors: i64,
            unique_sessions: i64,
        }

        let stats = EventStats::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &stats_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .one(self.db.as_ref())
        .await?
        .unwrap_or(EventStats {
            total_count: 0,
            unique_visitors: 0,
            unique_sessions: 0,
        });

        // 2. Get time series data
        let activity_sql = format!(
            r#"
            WITH time_buckets AS (
                SELECT generate_series(
                    date_trunc('{date_trunc}', $3::timestamptz),
                    date_trunc('{date_trunc}', $4::timestamptz),
                    '{pg_interval}'::interval
                ) AS bucket
            ),
            event_activity AS (
                SELECT
                    date_trunc('{date_trunc}', e.timestamp) as bucket,
                    COUNT(*) as count,
                    COUNT(DISTINCT e.visitor_id) as unique_visitors
                FROM events e
                WHERE e.project_id = $1
                  AND COALESCE(e.event_name, e.event_type) = $2
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND {not_filtered}
                  {env_filter}
                GROUP BY date_trunc('{date_trunc}', e.timestamp)
            )
            SELECT
                tb.bucket::timestamptz as timestamp,
                COALESCE(ea.count, 0) as count,
                COALESCE(ea.unique_visitors, 0) as unique_visitors
            FROM time_buckets tb
            LEFT JOIN event_activity ea ON tb.bucket = ea.bucket
            ORDER BY tb.bucket
            "#,
            date_trunc = date_trunc_unit,
            pg_interval = pg_interval,
            env_filter = env_filter,
        );

        #[derive(FromQueryResult)]
        struct ActivityBucket {
            timestamp: UtcDateTime,
            count: i64,
            unique_visitors: i64,
        }

        let activity_results = ActivityBucket::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &activity_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let activity_over_time: Vec<crate::types::responses::EventActivityBucket> =
            activity_results
                .into_iter()
                .map(|b| crate::types::responses::EventActivityBucket {
                    timestamp: b.timestamp,
                    count: b.count,
                    unique_visitors: b.unique_visitors,
                })
                .collect();

        // 3. Get top referrers
        let referrers_sql = format!(
            r#"
            WITH referrer_stats AS (
                SELECT
                    COALESCE(NULLIF(e.referrer_hostname, ''), 'Direct') as referrer,
                    COUNT(*) as count
                FROM events e
                WHERE e.project_id = $1
                  AND COALESCE(e.event_name, e.event_type) = $2
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND {not_filtered}
                  {}
                GROUP BY COALESCE(NULLIF(e.referrer_hostname, ''), 'Direct')
            ),
            total AS (
                SELECT SUM(count) as total_count FROM referrer_stats
            )
            SELECT
                rs.referrer,
                rs.count,
                CASE WHEN t.total_count > 0
                     THEN rs.count::float / t.total_count::float * 100
                     ELSE 0 END as percentage
            FROM referrer_stats rs
            CROSS JOIN total t
            ORDER BY rs.count DESC
            LIMIT 20
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct ReferrerResult {
            referrer: String,
            count: i64,
            percentage: f64,
        }

        let referrer_results = ReferrerResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &referrers_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let referrers: Vec<crate::types::responses::EventReferrerStats> = referrer_results
            .into_iter()
            .map(|r| crate::types::responses::EventReferrerStats {
                referrer: r.referrer,
                count: r.count,
                percentage: r.percentage,
            })
            .collect();

        // 4. Get top countries
        let countries_sql = format!(
            r#"
            WITH country_stats AS (
                SELECT
                    COALESCE(ig.country, 'Unknown') as country,
                    ig.country_code,
                    COUNT(*) as count
                FROM events e
                LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
                WHERE e.project_id = $1
                  AND COALESCE(e.event_name, e.event_type) = $2
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND {not_filtered}
                  {}
                GROUP BY COALESCE(ig.country, 'Unknown'), ig.country_code
            ),
            total AS (
                SELECT SUM(count) as total_count FROM country_stats
            )
            SELECT
                cs.country,
                cs.country_code,
                cs.count,
                CASE WHEN t.total_count > 0
                     THEN cs.count::float / t.total_count::float * 100
                     ELSE 0 END as percentage
            FROM country_stats cs
            CROSS JOIN total t
            ORDER BY cs.count DESC
            LIMIT 30
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct CountryResult {
            country: String,
            country_code: Option<String>,
            count: i64,
            percentage: f64,
        }

        let country_results = CountryResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &countries_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let countries: Vec<crate::types::responses::EventCountryStats> = country_results
            .into_iter()
            .map(|c| crate::types::responses::EventCountryStats {
                country: c.country,
                country_code: c.country_code,
                count: c.count,
                percentage: c.percentage,
            })
            .collect();

        // 5. Get top browsers
        let browsers_sql = format!(
            r#"
            WITH browser_stats AS (
                SELECT
                    COALESCE(e.browser, 'Unknown') as browser,
                    COUNT(*) as count
                FROM events e
                WHERE e.project_id = $1
                  AND COALESCE(e.event_name, e.event_type) = $2
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND {not_filtered}
                  {}
                GROUP BY COALESCE(e.browser, 'Unknown')
            ),
            total AS (
                SELECT SUM(count) as total_count FROM browser_stats
            )
            SELECT
                bs.browser,
                bs.count,
                CASE WHEN t.total_count > 0
                     THEN bs.count::float / t.total_count::float * 100
                     ELSE 0 END as percentage
            FROM browser_stats bs
            CROSS JOIN total t
            ORDER BY bs.count DESC
            LIMIT 20
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct BrowserResult {
            browser: String,
            count: i64,
            percentage: f64,
        }

        let browser_results = BrowserResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &browsers_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let browsers: Vec<crate::types::responses::EventBrowserStats> = browser_results
            .into_iter()
            .map(|b| crate::types::responses::EventBrowserStats {
                browser: b.browser,
                count: b.count,
                percentage: b.percentage,
            })
            .collect();

        Ok(crate::types::responses::EventDetailResponse {
            event_name: event_name.to_string(),
            total_count: stats.total_count,
            unique_visitors: stats.unique_visitors,
            unique_sessions: stats.unique_sessions,
            activity_over_time,
            referrers,
            countries,
            browsers,
            bucket_interval: interval.to_string(),
        })
    }

    /// Get paginated list of visitors who triggered a specific event
    async fn get_event_visitors(
        &self,
        project_id: i32,
        event_name: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<crate::types::responses::EventVisitorsResponse, AnalyticsError> {
        // Traffic-filter exclusions only, as in the event counts.
        let not_filtered = temps_entities::events::not_excluded_by_traffic_filter("e");
        let per_page = per_page.min(100);
        let offset = (page.saturating_sub(1)) * per_page;

        let env_filter = environment_id
            .map(|id| format!("AND e.environment_id = {}", id))
            .unwrap_or_default();

        // Get total count of unique visitors
        let count_sql = format!(
            r#"
            SELECT COUNT(DISTINCT e.visitor_id) as total_count
            FROM events e
            WHERE e.project_id = $1
              AND COALESCE(e.event_name, e.event_type) = $2
              AND e.timestamp >= $3
              AND e.timestamp < $4
              AND {not_filtered}
              AND e.visitor_id IS NOT NULL
              {}
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct CountResult {
            total_count: i64,
        }

        let total_count = CountResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &count_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .one(self.db.as_ref())
        .await?
        .map(|c| c.total_count)
        .unwrap_or(0);

        // Get paginated visitors with aggregated stats
        let visitors_sql = format!(
            r#"
            WITH visitor_events AS (
                SELECT
                    e.visitor_id,
                    COUNT(*) as event_count,
                    MIN(e.timestamp) as first_triggered,
                    MAX(e.timestamp) as last_triggered,
                    -- Pick the most recent non-null values for each field
                    (array_agg(ig.country ORDER BY e.timestamp DESC) FILTER (WHERE ig.country IS NOT NULL))[1] as country,
                    (array_agg(ig.country_code ORDER BY e.timestamp DESC) FILTER (WHERE ig.country_code IS NOT NULL))[1] as country_code,
                    (array_agg(ig.city ORDER BY e.timestamp DESC) FILTER (WHERE ig.city IS NOT NULL))[1] as city,
                    (array_agg(e.browser ORDER BY e.timestamp DESC) FILTER (WHERE e.browser IS NOT NULL))[1] as browser,
                    (array_agg(e.device_type ORDER BY e.timestamp DESC) FILTER (WHERE e.device_type IS NOT NULL))[1] as device_type,
                    (array_agg(e.referrer_hostname ORDER BY e.timestamp DESC) FILTER (WHERE e.referrer_hostname IS NOT NULL AND e.referrer_hostname != ''))[1] as referrer_hostname
                FROM events e
                LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
                WHERE e.project_id = $1
                  AND COALESCE(e.event_name, e.event_type) = $2
                  AND e.timestamp >= $3
                  AND e.timestamp < $4
                  AND {not_filtered}
                  AND e.visitor_id IS NOT NULL
                  {env_filter}
                GROUP BY e.visitor_id
                ORDER BY event_count DESC, last_triggered DESC
                LIMIT $5 OFFSET $6
            )
            SELECT
                ve.visitor_id,
                COALESCE(v.visitor_id, '') as visitor_uuid,
                ve.event_count,
                ve.first_triggered,
                ve.last_triggered,
                ve.country,
                ve.country_code,
                ve.city,
                ve.browser,
                ve.device_type,
                ve.referrer_hostname
            FROM visitor_events ve
            LEFT JOIN visitor v ON v.id = ve.visitor_id
            ORDER BY ve.event_count DESC, ve.last_triggered DESC
            "#,
            env_filter = env_filter
        );

        #[derive(FromQueryResult)]
        struct VisitorResult {
            visitor_id: i32,
            visitor_uuid: String,
            event_count: i64,
            first_triggered: UtcDateTime,
            last_triggered: UtcDateTime,
            country: Option<String>,
            country_code: Option<String>,
            city: Option<String>,
            browser: Option<String>,
            device_type: Option<String>,
            referrer_hostname: Option<String>,
        }

        let visitor_results = VisitorResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &visitors_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
                (per_page as i64).into(),
                (offset as i64).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let visitors: Vec<crate::types::responses::EventVisitorInfo> = visitor_results
            .into_iter()
            .map(|v| crate::types::responses::EventVisitorInfo {
                visitor_id: v.visitor_id,
                visitor_uuid: v.visitor_uuid,
                event_count: v.event_count,
                first_triggered: v.first_triggered,
                last_triggered: v.last_triggered,
                country: v.country,
                country_code: v.country_code,
                city: v.city,
                browser: v.browser,
                device_type: v.device_type,
                referrer_hostname: v.referrer_hostname,
            })
            .collect();

        Ok(crate::types::responses::EventVisitorsResponse {
            event_name: event_name.to_string(),
            total_count,
            page,
            per_page,
            visitors,
        })
    }

    async fn get_event_entries(
        &self,
        project_id: i32,
        event_name: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<crate::types::responses::EventEntriesResponse, AnalyticsError> {
        // Traffic-filter exclusions only, as in the event counts.
        let not_filtered = temps_entities::events::not_excluded_by_traffic_filter("e");
        let per_page = per_page.min(100);
        let offset = (page.saturating_sub(1)) * per_page;

        let env_filter = environment_id
            .map(|id| format!("AND e.environment_id = {}", id))
            .unwrap_or_default();

        // Total number of occurrences in the range
        let count_sql = format!(
            r#"
            SELECT COUNT(*) as total_count
            FROM events e
            WHERE e.project_id = $1
              AND COALESCE(e.event_name, e.event_type) = $2
              AND e.timestamp >= $3
              AND e.timestamp < $4
              AND {not_filtered}
              {}
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct CountResult {
            total_count: i64,
        }

        let total_count = CountResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &count_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
            ],
        ))
        .one(self.db.as_ref())
        .await?
        .map(|c| c.total_count)
        .unwrap_or(0);

        // Paginated raw occurrences, most recent first, with props JSON
        let entries_sql = format!(
            r#"
            SELECT
                e.id,
                e.timestamp,
                e.visitor_id,
                v.visitor_id as visitor_uuid,
                e.session_id,
                e.page_path,
                e.href,
                e.browser,
                e.device_type,
                ig.country,
                ig.country_code,
                ig.city,
                e.props
            FROM events e
            LEFT JOIN visitor v ON v.id = e.visitor_id
            LEFT JOIN ip_geolocations ig ON e.ip_geolocation_id = ig.id
            WHERE e.project_id = $1
              AND COALESCE(e.event_name, e.event_type) = $2
              AND e.timestamp >= $3
              AND e.timestamp < $4
              AND {not_filtered}
              {env_filter}
            ORDER BY e.timestamp DESC, e.id DESC
            LIMIT $5 OFFSET $6
            "#,
            env_filter = env_filter
        );

        #[derive(FromQueryResult)]
        struct EntryResult {
            id: i64,
            timestamp: UtcDateTime,
            visitor_id: Option<i32>,
            visitor_uuid: Option<String>,
            session_id: Option<String>,
            page_path: String,
            href: String,
            browser: Option<String>,
            device_type: Option<String>,
            country: Option<String>,
            country_code: Option<String>,
            city: Option<String>,
            props: Option<serde_json::Value>,
        }

        let entry_results = EntryResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &entries_sql,
            vec![
                project_id.into(),
                event_name.into(),
                start_date.into(),
                end_date.into(),
                (per_page as i64).into(),
                (offset as i64).into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;

        let entries: Vec<crate::types::responses::EventEntryInfo> = entry_results
            .into_iter()
            .map(|e| crate::types::responses::EventEntryInfo {
                id: e.id,
                timestamp: e.timestamp,
                visitor_id: e.visitor_id,
                visitor_uuid: e.visitor_uuid,
                session_id: e.session_id,
                page_path: e.page_path,
                href: e.href,
                browser: e.browser,
                device_type: e.device_type,
                country: e.country,
                country_code: e.country_code,
                city: e.city,
                props: e.props,
            })
            .collect();

        Ok(crate::types::responses::EventEntriesResponse {
            event_name: event_name.to_string(),
            total_count,
            page,
            per_page,
            entries,
        })
    }
}

#[cfg(test)]
mod tests;
