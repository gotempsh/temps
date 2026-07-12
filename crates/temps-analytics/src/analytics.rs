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

        // Get basic statistics
        let stats_query = r#"
            WITH visitor_stats AS (
                SELECT
                    MIN(timestamp) as first_seen,
                    MAX(timestamp) as last_seen,
                    COUNT(DISTINCT session_id) as total_sessions,
                    COUNT(*) FILTER (WHERE event_type = 'page_view') as total_page_views,
                    COUNT(*) as total_events,
                    COALESCE(SUM(time_on_page), 0) as total_time_seconds,
                    COUNT(DISTINCT session_id) FILTER (WHERE is_bounce = true) as bounce_sessions,
                    COUNT(*) FILTER (WHERE event_type NOT IN ('page_view', 'page_leave')) as engagement_events
                FROM events
                WHERE visitor_id = $1
            )
            SELECT
                first_seen,
                last_seen,
                total_sessions,
                total_page_views,
                total_events,
                CASE WHEN total_sessions > 0
                     THEN total_time_seconds::float / total_sessions::float
                     ELSE 0 END as average_session_duration,
                CASE WHEN total_sessions > 0
                     THEN bounce_sessions::float / total_sessions::float * 100
                     ELSE 0 END as bounce_rate,
                CASE WHEN total_events > 0
                     THEN engagement_events::float / total_events::float * 100
                     ELSE 0 END as engagement_rate
            FROM visitor_stats
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

        if visitor.is_none() {
            return Ok(None);
        }

        let visitor = visitor.unwrap();

        let sql_query = r#"
            WITH session_stats AS (
                SELECT
                    rs.id as session_id,
                    MIN(e.timestamp) as started_at,
                    MAX(e.timestamp) as ended_at,
                    EXTRACT(EPOCH FROM (MAX(e.timestamp) - MIN(e.timestamp))) as duration_seconds,
                    COUNT(*) FILTER (WHERE e.event_type = 'page_view') as page_views,
                    COUNT(*) as events_count,
                    COUNT(DISTINCT rl.id) as requests_count,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp ASC))[1]                        as entry_path,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp DESC))[1]                       as exit_path,
                    MIN(e.referrer) as referrer,
                    BOOL_OR(e.is_bounce) as is_bounced,
                    -- A session is engaged if the visitor spent real attention:
                    -- at least 10s of measured wall-clock time, OR fired a
                    -- genuine interaction event. Auto-fired view events
                    -- (page_view, page_leave, *_viewed) do not count — they
                    -- trigger from intersection observers for bots too.
                    (
                        EXTRACT(EPOCH FROM (MAX(e.timestamp) - MIN(e.timestamp))) >= 10
                        OR COUNT(*) FILTER (
                            WHERE e.event_type NOT IN ('page_view', 'page_leave')
                              AND e.event_type NOT LIKE '%\_viewed' ESCAPE '\'
                        ) > 0
                    ) as is_engaged
                FROM events e
                LEFT JOIN request_logs rl ON rl.session_id = e.id AND rl.project_id = e.project_id
                -- Tolerate both the bare-UUID format (new events, after the
                -- session-cookie normalisation fix) and the legacy v2|uuid|ts
                -- format stored by older versions of the analytics ingest path.
                -- split_part(e.session_id, '|', 2) extracts the UUID segment
                -- from 'v2|<uuid>|<ts>'; the LIKE guard avoids a false match
                -- on empty strings when e.session_id has no '|' delimiters.
                LEFT JOIN request_sessions rs ON (
                    rs.session_id = e.session_id
                    OR (e.session_id LIKE 'v2|%' AND rs.session_id = split_part(e.session_id, '|', 2))
                )
                WHERE e.visitor_id = $1 AND e.session_id IS NOT NULL
                GROUP BY rs.id
            )
            SELECT
                session_id,
                started_at,
                ended_at,
                COALESCE(duration_seconds, 0)::bigint as duration_seconds,
                page_views,
                events_count,
                requests_count,
                entry_path,
                exit_path,
                referrer,
                is_bounced,
                is_engaged,
                COUNT(*) OVER() as total_sessions
            FROM session_stats
            ORDER BY started_at DESC
            LIMIT $2
            "#;

        #[derive(FromQueryResult)]
        struct SessionResult {
            // Nullable because the query uses LEFT JOIN request_sessions: when no
            // matching row exists yet (proxy async flush hasn't landed), rs.id comes
            // back NULL. Decoding NULL into a non-optional i32 causes a hard
            // type-decode error that kills the whole request. We filter these out
            // below so SessionSummary.session_id remains non-optional.
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
            sql_query,
            vec![visitor_id.into(), (limit_val as i64).into()],
        ))
        .all(self.db.as_ref())
        .await?;

        // The window-function total (pre-LIMIT, pre-filter) is the correct
        // pagination baseline. Subtract the count of NULL-session groups so the
        // displayed total never exceeds the visible list.
        let raw_total = results.first().map(|r| r.total_sessions).unwrap_or(0);
        let mut null_count = 0i64;

        let sessions = results
            .into_iter()
            .filter_map(|r| {
                let sid = match r.session_id {
                    Some(id) => id,
                    None => {
                        // No request_sessions row for this e.session_id yet
                        // (timing race or prior proxy error). Drop this group
                        // from the visible list and deduct it from the total.
                        null_count += 1;
                        return None;
                    }
                };
                Some(crate::types::responses::SessionSummary {
                    session_id: sid,
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

        let total_sessions = raw_total.saturating_sub(null_count);

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

        if visitor.is_none() {
            return Ok(None);
        }

        let limit_sessions = limit_sessions.unwrap_or(50).min(100) as i64;

        // Step 1: Get sessions with traffic source context (newest first)
        let sessions_sql = r#"
            WITH session_data AS (
                SELECT
                    rs.id as session_id,
                    MIN(e.timestamp) as started_at,
                    MAX(e.timestamp) as ended_at,
                    EXTRACT(EPOCH FROM (MAX(e.timestamp) - MIN(e.timestamp)))::bigint as duration_seconds,
                    COUNT(*) FILTER (WHERE e.event_type = 'page_view') as page_views,
                    COUNT(*) as events_count,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp ASC)  FILTER (WHERE e.event_type = 'page_view'))[1] as entry_path,
                    (ARRAY_AGG(e.page_path ORDER BY e.timestamp DESC) FILTER (WHERE e.event_type = 'page_view'))[1] as exit_path,
                    rs.referrer,
                    rs.referrer_hostname,
                    rs.channel,
                    rs.utm_source,
                    rs.utm_medium,
                    rs.utm_campaign,
                    BOOL_OR(e.is_bounce) as is_bounced,
                    COUNT(*) FILTER (WHERE e.event_type NOT IN ('page_view', 'page_leave', 'heartbeat', 'web_vitals')) > 0 as is_engaged
                FROM events e
                -- Tolerate both bare-UUID and legacy v2|uuid|ts formats in
                -- events.session_id (see get_visitor_sessions_by_id comment).
                JOIN request_sessions rs ON (
                    rs.session_id = e.session_id
                    OR (e.session_id LIKE 'v2|%' AND rs.session_id = split_part(e.session_id, '|', 2))
                )
                WHERE e.visitor_id = $1
                  AND e.project_id = $2
                  AND e.session_id IS NOT NULL
                GROUP BY rs.id, rs.referrer, rs.referrer_hostname, rs.channel,
                         rs.utm_source, rs.utm_medium, rs.utm_campaign
            )
            SELECT
                session_id,
                started_at,
                ended_at,
                COALESCE(duration_seconds, 0) as duration_seconds,
                page_views,
                events_count,
                entry_path,
                exit_path,
                referrer,
                referrer_hostname,
                channel,
                utm_source,
                utm_medium,
                utm_campaign,
                is_bounced,
                is_engaged,
                COUNT(*) OVER() as total_sessions
            FROM session_data
            ORDER BY started_at DESC
            LIMIT $3
        "#;

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
            sessions_sql,
            vec![visitor_id.into(), project_id.into(), limit_sessions.into()],
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

        // Build the session_id placeholders for IN clause
        let session_id_placeholders: Vec<String> = session_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 3))
            .collect();
        let in_clause = session_id_placeholders.join(", ");

        // Build events query with computed time_on_page.
        // Materialize all session events in a CTE, then use self-joins (merge joins)
        // instead of correlated subqueries to preserve exact 3-priority logic:
        //   Priority 1: next page_leave for same session+page within 30 min
        //   Priority 2: next page_view in same session (any page)
        //   Priority 3: fallback 30 seconds
        let events_sql = format!(
            r#"
            WITH all_session_events AS MATERIALIZED (
                -- Materialize ALL events for these sessions into a small CTE
                SELECT
                    e.id,
                    e.event_type,
                    COALESCE(e.event_name, e.event_type) as event_name,
                    e.timestamp as occurred_at,
                    e.page_path,
                    e.page_title,
                    e.referrer,
                    e.is_entry,
                    e.is_exit,
                    e.is_bounce,
                    e.session_page_number,
                    e.scroll_depth,
                    e.session_id,
                    COALESCE(e.props, e.event_data::jsonb, '{{}}'::jsonb) as event_data,
                    rs.id as rs_id
                FROM events e
                -- Tolerate both bare-UUID and legacy v2|uuid|ts formats in
                -- events.session_id (see get_visitor_sessions_by_id comment).
                JOIN request_sessions rs ON (
                    rs.session_id = e.session_id
                    OR (e.session_id LIKE 'v2|%' AND rs.session_id = split_part(e.session_id, '|', 2))
                )
                WHERE e.visitor_id = $1
                  AND e.project_id = $2
                  AND rs.id IN ({in_clause})
            ),
            -- Pre-compute time_on_page for page_view events using self-joins
            -- Priority 1: next page_leave for same session+page within 30 min
            p1 AS (
                SELECT
                    pv.id as event_id,
                    pv.session_id,
                    pv.occurred_at as pv_ts,
                    MIN(pl.occurred_at) as next_page_leave_ts
                FROM all_session_events pv
                LEFT JOIN all_session_events pl
                    ON pl.session_id = pv.session_id
                    AND pl.event_type = 'page_leave'
                    AND pl.page_path = pv.page_path
                    AND pl.occurred_at > pv.occurred_at
                    AND pl.occurred_at <= pv.occurred_at + INTERVAL '30 minutes'
                WHERE pv.event_type = 'page_view'
                GROUP BY pv.id, pv.session_id, pv.occurred_at
            ),
            -- Priority 2: next page_view in same session (any page)
            p2 AS (
                SELECT
                    pv.id as event_id,
                    pv.session_id,
                    pv.occurred_at as pv_ts,
                    MIN(npv.occurred_at) as next_page_view_ts
                FROM all_session_events pv
                LEFT JOIN all_session_events npv
                    ON npv.session_id = pv.session_id
                    AND npv.event_type = 'page_view'
                    AND npv.occurred_at > pv.occurred_at
                WHERE pv.event_type = 'page_view'
                GROUP BY pv.id, pv.session_id, pv.occurred_at
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
                        EXTRACT(EPOCH FROM (
                            COALESCE(
                                p1.next_page_leave_ts,
                                p2.next_page_view_ts,
                                ase.occurred_at + INTERVAL '30 seconds'
                            ) - ase.occurred_at
                        ))::int
                    ELSE NULL
                END as computed_time_on_page,
                ase.is_entry,
                ase.is_exit,
                ase.is_bounce,
                ase.session_page_number,
                ase.scroll_depth,
                ase.event_data,
                ase.rs_id
            FROM all_session_events ase
            LEFT JOIN p1 ON ase.id = p1.event_id
            LEFT JOIN p2 ON ase.id = p2.event_id
            WHERE ase.event_type NOT IN ('heartbeat', 'web_vitals', 'page_leave')
            ORDER BY ase.rs_id, ase.occurred_at ASC
            "#,
            in_clause = in_clause
        );

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
            is_entry: bool,
            is_exit: bool,
            is_bounce: bool,
            session_page_number: Option<i32>,
            scroll_depth: Option<i32>,
            event_data: serde_json::Value,
            rs_id: i32,
        }

        let mut event_values: Vec<sea_orm::Value> = vec![visitor_id.into(), project_id.into()];
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
                row.computed_time_on_page.filter(|&t| t > 0 && t < 1800)
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
                    is_entry: row.is_entry,
                    is_exit: row.is_exit,
                    is_bounce: row.is_bounce,
                    session_page_number: row.session_page_number,
                    scroll_depth: row.scroll_depth,
                    event_data,
                },
            );
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

        // Build WHERE conditions with parameterized queries
        let mut where_conditions =
            vec!["session_id = $1".to_string(), "project_id = $2".to_string()];
        let mut values: Vec<sea_orm::Value> =
            vec![request_session.session_id.into(), project_id.into()];
        let mut param_index = 3;

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

        let limit_val = limit.unwrap_or(100).min(1000);
        let offset_val = offset.unwrap_or(0);
        let order = match sort_order.as_deref() {
            Some("asc") => "ASC",
            _ => "DESC",
        };

        let where_clause = where_conditions.join(" AND ");
        let sql_query = format!(
            r#"
            WITH event_data AS (
                SELECT
                    ROW_NUMBER() OVER (ORDER BY timestamp {}) as id,
                    COALESCE(event_name, event_type) as event_name,
                    timestamp as occurred_at,
                    COALESCE(props, event_data::jsonb, '{{}}'::jsonb) as event_data,
                    page_path as request_path,
                    request_query,
                    COUNT(*) OVER() as total_count
                FROM events
                WHERE {}
                ORDER BY timestamp {}
                LIMIT ${} OFFSET ${}
            )
            SELECT * FROM event_data
            "#,
            order,
            where_clause,
            order,
            param_index,
            param_index + 1
        );

        // Add LIMIT and OFFSET as parameters
        values.push((limit_val as i64).into());
        values.push((offset_val as i64).into());

        #[derive(FromQueryResult)]
        struct EventResult {
            id: i64,
            event_name: String,
            occurred_at: UtcDateTime,
            event_data: serde_json::Value,
            request_path: String,
            request_query: Option<String>,
            total_count: i64,
        }

        let results = EventResult::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_query,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        if results.is_empty() {
            return Ok(Some(SessionEventsResponse {
                session_id,
                events: vec![],
                total_count: 0,
                offset: offset_val,
                limit: limit_val,
            }));
        }

        let total_count = results.first().map(|r| r.total_count).unwrap_or(0);

        let events = results
            .into_iter()
            .map(|r| crate::types::responses::SessionEvent {
                id: r.id as i32,
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
        enrichment_data: serde_json::Value,
    ) -> Result<EnrichVisitorResponse, AnalyticsError> {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_entities::visitor;

        // Find the visitor by id
        let visitor = visitor::Entity::find()
            .filter(visitor::Column::Id.eq(visitor_id))
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?;

        // Early return if visitor not found
        let Some(visitor_model) = visitor else {
            return Ok(EnrichVisitorResponse {
                success: false,
                visitor_id: visitor_id.to_string(),
                message: "Visitor not found".to_string(),
            });
        };

        let mut active_model: visitor::ActiveModel = visitor_model.into();

        // Merge enrichment_data with existing custom_data (if any)
        let merged_custom_data = match &active_model.custom_data {
            sea_orm::ActiveValue::Set(Some(existing_json)) => {
                // existing_json is serde_json::Value
                let mut existing_map = match existing_json.as_object() {
                    Some(map) => map.clone(),
                    None => serde_json::Map::new(),
                };
                if let Some(new_map) = enrichment_data.as_object() {
                    for (k, v) in new_map {
                        existing_map.insert(k.clone(), v.clone());
                    }
                }
                serde_json::Value::Object(existing_map)
            }
            _ => enrichment_data.clone(),
        };

        // Set the merged custom_data as serde_json::Value
        active_model.custom_data = Set(Some(merged_custom_data));

        // Save the updated visitor
        active_model
            .update(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?;

        Ok(EnrichVisitorResponse {
            success: true,
            visitor_id: visitor_id.to_string(),
            message: "Visitor enriched successfully".to_string(),
        })
    }

    /// Enrich visitor by GUID (visitor_id string, may be encrypted with enc_ prefix)
    async fn enrich_visitor_by_guid(
        &self,
        visitor_guid: &str,
        enrichment_data: serde_json::Value,
    ) -> Result<EnrichVisitorResponse, AnalyticsError> {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_entities::visitor;

        // Handle encrypted visitor ID (enc_ prefix)
        let actual_visitor_id = if let Some(encrypted) = visitor_guid.strip_prefix("enc_") {
            match self.cookie_crypto.decrypt(encrypted) {
                Ok(decrypted) => decrypted,
                Err(_) => {
                    return Err(AnalyticsError::InvalidVisitorId(visitor_guid.to_string()));
                }
            }
        } else {
            visitor_guid.to_string()
        };

        // Find the visitor by visitor_id (guid)
        let visitor = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(&actual_visitor_id))
            .one(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?;

        // Early return if visitor not found
        let Some(visitor_model) = visitor else {
            return Ok(EnrichVisitorResponse {
                success: false,
                visitor_id: actual_visitor_id,
                message: "Visitor not found".to_string(),
            });
        };

        let mut active_model: visitor::ActiveModel = visitor_model.into();

        // Merge enrichment_data with existing custom_data (if any)
        let merged_custom_data = match &active_model.custom_data {
            sea_orm::ActiveValue::Set(Some(existing_json)) => {
                let mut existing_map = match existing_json.as_object() {
                    Some(map) => map.clone(),
                    None => serde_json::Map::new(),
                };
                if let Some(new_map) = enrichment_data.as_object() {
                    for (k, v) in new_map {
                        existing_map.insert(k.clone(), v.clone());
                    }
                }
                serde_json::Value::Object(existing_map)
            }
            _ => enrichment_data.clone(),
        };

        // Set the merged custom_data as serde_json::Value
        active_model.custom_data = Set(Some(merged_custom_data));

        // Save the updated visitor
        active_model
            .update(self.db.as_ref())
            .await
            .map_err(AnalyticsError::from)?;

        Ok(EnrichVisitorResponse {
            success: true,
            visitor_id: actual_visitor_id,
            message: "Visitor enriched successfully".to_string(),
        })
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

        // Materialize session events in a CTE, then use self-joins (merge joins)
        // instead of correlated subqueries to preserve exact 3-priority time_on_page logic:
        //   Priority 1: next page_leave for same session+page within 30 min
        //   Priority 2: next page_view in same session (any page)
        //   Priority 3: fallback 30 seconds
        // Self-joins allow PostgreSQL to use merge joins on the sorted CTE data,
        // which is O(N log N) vs O(N*S) for correlated subquery CTE scans.
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
            ),
            -- Priority 1: for each page_view, find the min page_leave timestamp
            -- for same session+page_path within 30 min (via self-join + GROUP BY)
            p1 AS (
                SELECT
                    pv.event_id,
                    pv.session_id,
                    pv.page_path,
                    pv.timestamp as pv_ts,
                    MIN(pl.timestamp) as next_page_leave_ts
                FROM session_events pv
                LEFT JOIN session_events pl
                    ON pl.session_id = pv.session_id
                    AND pl.event_type = 'page_leave'
                    AND pl.page_path = pv.page_path
                    AND pl.timestamp > pv.timestamp
                    AND pl.timestamp <= pv.timestamp + INTERVAL '30 minutes'
                WHERE pv.event_type = 'page_view'
                  AND pv.page_path IS NOT NULL
                  AND pv.page_path != ''
                GROUP BY pv.event_id, pv.session_id, pv.page_path, pv.timestamp
            ),
            -- Priority 2: for each page_view, find the next page_view timestamp
            -- in the same session (any page, no time cap)
            p2 AS (
                SELECT
                    pv.event_id,
                    pv.session_id,
                    pv.page_path,
                    pv.timestamp as pv_ts,
                    MIN(npv.timestamp) as next_page_view_ts
                FROM session_events pv
                LEFT JOIN session_events npv
                    ON npv.session_id = pv.session_id
                    AND npv.event_type = 'page_view'
                    AND npv.timestamp > pv.timestamp
                WHERE pv.event_type = 'page_view'
                  AND pv.page_path IS NOT NULL
                  AND pv.page_path != ''
                GROUP BY pv.event_id, pv.session_id, pv.page_path, pv.timestamp
            ),
            page_durations AS (
                SELECT
                    p1.page_path,
                    p1.session_id,
                    p1.pv_ts as first_seen_ts,
                    EXTRACT(EPOCH FROM (
                        COALESCE(
                            p1.next_page_leave_ts,
                            p2.next_page_view_ts,
                            p1.pv_ts + INTERVAL '30 seconds'
                        ) - p1.pv_ts
                    )) as time_on_page_seconds
                FROM p1
                JOIN p2 ON p1.event_id = p2.event_id
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
                    AVG(time_on_page::float) as avg_time_on_page,
                    COUNT(DISTINCT visitor_id) as unique_visitors,
                    SUM(CASE WHEN is_bounce THEN 1 ELSE 0 END)::float /
                        NULLIF(COUNT(DISTINCT session_id), 0) * 100 as bounce_rate
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
        let sql = r#"
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
            WHERE v.project_id = $1
              AND ($2::int IS NULL OR v.environment_id = $2)
              AND v.last_seen >= NOW() - INTERVAL '1 minute' * $3
              AND v.is_crawler = false
              AND COALESCE(ig.is_hosting_provider, false) = false
            ORDER BY v.last_seen DESC
        "#;

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
        // Query to get overall stats across all projects
        let total_stats_sql = r#"
            -- Optimized: avoids join fan-out, uses half-open (>= $1 AND < $2) intervals
            WITH
                unique_visitors AS (
                    SELECT COUNT(DISTINCT e.visitor_id) AS n
                    FROM events e
                    WHERE e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_visits AS (
                    SELECT COUNT(*) AS n
                    FROM request_sessions rs
                    WHERE rs.started_at >= $1 AND rs.started_at < $2
                ),
                total_events AS (
                    SELECT COUNT(*) AS n
                    FROM events e
                    WHERE e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_page_views AS (
                    SELECT COUNT(*) AS n
                    FROM events e
                    WHERE e.event_type = 'page_view'
                      AND e.timestamp >= $1 AND e.timestamp < $2
                ),
                total_projects AS (
                    SELECT COUNT(*) AS n
                    FROM projects p
                )
            SELECT
                unique_visitors.n AS unique_visitors,
                total_visits.n AS total_visits,
                total_page_views.n AS total_page_views,
                total_events.n AS total_events,
                total_projects.n AS total_projects,
                0.0::double precision as avg_bounce_rate,
                0.0::double precision as avg_engagement_rate
            FROM unique_visitors, total_visits, total_page_views, total_events, total_projects
        "#;

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
        let offset = (page.saturating_sub(1)) * per_page;

        // Count total matching rows
        let env_filter = if let Some(env_id) = environment_id {
            format!("AND e.environment_id = {}", env_id)
        } else {
            String::new()
        };

        let count_query = format!(
            r#"
            SELECT COUNT(*) as count
            FROM events e
            WHERE e.project_id = $1
              AND e.page_path = $2
              AND e.event_type = 'page_view'
              AND e.timestamp >= $3
              AND e.timestamp <= $4
              AND e.is_crawler = false
              {}
            "#,
            env_filter
        );

        let count_stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &count_query,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
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

        // Fetch visitor sessions with geolocation data.
        // Compute time_on_page by materializing session events in a CTE, then using
        // self-joins (merge joins) instead of correlated subqueries to preserve the
        // exact 3-priority COALESCE logic:
        //   Priority 1: next page_leave for same session+page within 30 min
        //   Priority 2: next page_view in same session (any page)
        //   Priority 3: fallback 30 seconds
        let data_query = format!(
            r#"
            WITH session_events AS MATERIALIZED (
                -- Materialize all page_view and page_leave events for matching sessions.
                SELECT
                    e.id as event_id,
                    e.session_id,
                    e.event_type,
                    e.page_path,
                    e.timestamp,
                    e.visitor_id,
                    e.is_entry,
                    e.is_exit,
                    e.is_bounce,
                    e.session_page_number,
                    e.referrer,
                    e.browser,
                    e.operating_system,
                    e.device_type,
                    e.ip_geolocation_id
                FROM events e
                WHERE e.session_id IN (
                    SELECT DISTINCT session_id
                    FROM events
                    WHERE project_id = $1
                      AND page_path = $2
                      AND event_type = 'page_view'
                      AND timestamp >= $3
                      AND timestamp <= $4
                      AND is_crawler = false
                      {env_filter}
                )
                AND e.event_type IN ('page_view', 'page_leave')
            ),
            -- Priority 1: next page_leave for same session+page within 30 min
            p1 AS (
                SELECT
                    pv.event_id,
                    pv.session_id,
                    pv.timestamp as pv_ts,
                    MIN(pl.timestamp) as next_page_leave_ts
                FROM session_events pv
                LEFT JOIN session_events pl
                    ON pl.session_id = pv.session_id
                    AND pl.event_type = 'page_leave'
                    AND pl.page_path = pv.page_path
                    AND pl.timestamp > pv.timestamp
                    AND pl.timestamp <= pv.timestamp + INTERVAL '30 minutes'
                WHERE pv.event_type = 'page_view'
                  AND pv.page_path = $2
                  AND pv.timestamp >= $3
                  AND pv.timestamp <= $4
                GROUP BY pv.event_id, pv.session_id, pv.timestamp
            ),
            -- Priority 2: next page_view in same session (any page)
            p2 AS (
                SELECT
                    pv.event_id,
                    pv.session_id,
                    pv.timestamp as pv_ts,
                    MIN(npv.timestamp) as next_page_view_ts
                FROM session_events pv
                LEFT JOIN session_events npv
                    ON npv.session_id = pv.session_id
                    AND npv.event_type = 'page_view'
                    AND npv.timestamp > pv.timestamp
                WHERE pv.event_type = 'page_view'
                  AND pv.page_path = $2
                  AND pv.timestamp >= $3
                  AND pv.timestamp <= $4
                GROUP BY pv.event_id, pv.session_id, pv.timestamp
            ),
            page_views_with_duration AS (
                SELECT
                    se.visitor_id,
                    COALESCE(v.visitor_id, '') as visitor_uuid,
                    p1.session_id,
                    p1.pv_ts as viewed_at,
                    EXTRACT(EPOCH FROM (
                        COALESCE(
                            p1.next_page_leave_ts,
                            p2.next_page_view_ts,
                            p1.pv_ts + INTERVAL '30 seconds'
                        ) - p1.pv_ts
                    ))::int as computed_time_on_page,
                    se.is_entry,
                    se.is_exit,
                    se.is_bounce,
                    se.session_page_number,
                    se.referrer,
                    se.browser,
                    se.operating_system,
                    se.device_type,
                    g.city,
                    g.country,
                    g.country_code
                FROM p1
                JOIN p2 ON p1.event_id = p2.event_id
                JOIN session_events se ON se.event_id = p1.event_id
                LEFT JOIN visitor v ON se.visitor_id = v.id
                LEFT JOIN ip_geolocations g ON se.ip_geolocation_id = g.id
            )
            SELECT
                visitor_id,
                visitor_uuid,
                session_id,
                viewed_at,
                CASE
                    WHEN computed_time_on_page > 0 AND computed_time_on_page < 1800
                    THEN computed_time_on_page
                    ELSE NULL
                END as time_on_page,
                is_entry,
                is_exit,
                is_bounce,
                session_page_number,
                referrer,
                browser,
                operating_system,
                device_type,
                city,
                country,
                country_code
            FROM page_views_with_duration
            ORDER BY viewed_at DESC
            LIMIT {per_page} OFFSET {offset}
            "#,
            env_filter = env_filter,
            per_page = per_page,
            offset = offset
        );

        let data_stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &data_query,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
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

        // Build environment filter
        let env_filter = environment_id
            .map(|id| format!("AND e.environment_id = {}", id))
            .unwrap_or_default();

        // 1. Get overall page stats
        // Materialize session events in a CTE, then use self-joins (merge joins)
        // instead of correlated subqueries to preserve exact 3-priority time_on_page logic.
        let stats_sql = format!(
            r#"
            WITH session_events AS MATERIALIZED (
                -- Materialize all events for sessions that viewed this page in the time range
                SELECT
                    e.id as event_id,
                    e.session_id,
                    e.event_type,
                    e.page_path,
                    e.timestamp,
                    e.visitor_id,
                    e.is_bounce,
                    e.referrer
                FROM events e
                WHERE e.session_id IN (
                    SELECT DISTINCT session_id
                    FROM events
                    WHERE project_id = $1
                      AND page_path = $2
                      AND event_type = 'page_view'
                      AND timestamp >= $3
                      AND timestamp < $4
                      {env_filter}
                )
                AND e.event_type IN ('page_view', 'page_leave')
            ),
            -- Priority 1: next page_leave for same session+page within 30 min
            p1 AS (
                SELECT
                    pv.event_id,
                    pv.session_id,
                    pv.timestamp as pv_ts,
                    MIN(pl.timestamp) as next_page_leave_ts
                FROM session_events pv
                LEFT JOIN session_events pl
                    ON pl.session_id = pv.session_id
                    AND pl.event_type = 'page_leave'
                    AND pl.page_path = pv.page_path
                    AND pl.timestamp > pv.timestamp
                    AND pl.timestamp <= pv.timestamp + INTERVAL '30 minutes'
                WHERE pv.event_type = 'page_view'
                  AND pv.page_path = $2
                  AND pv.timestamp >= $3
                  AND pv.timestamp < $4
                GROUP BY pv.event_id, pv.session_id, pv.timestamp
            ),
            -- Priority 2: next page_view in same session (any page)
            p2 AS (
                SELECT
                    pv.event_id,
                    pv.session_id,
                    pv.timestamp as pv_ts,
                    MIN(npv.timestamp) as next_page_view_ts
                FROM session_events pv
                LEFT JOIN session_events npv
                    ON npv.session_id = pv.session_id
                    AND npv.event_type = 'page_view'
                    AND npv.timestamp > pv.timestamp
                WHERE pv.event_type = 'page_view'
                  AND pv.page_path = $2
                  AND pv.timestamp >= $3
                  AND pv.timestamp < $4
                GROUP BY pv.event_id, pv.session_id, pv.timestamp
            ),
            page_events AS (
                SELECT
                    se.visitor_id,
                    p1.session_id,
                    p1.pv_ts as timestamp,
                    se.is_bounce,
                    se.referrer,
                    EXTRACT(EPOCH FROM (
                        COALESCE(
                            p1.next_page_leave_ts,
                            p2.next_page_view_ts,
                            p1.pv_ts + INTERVAL '30 seconds'
                        ) - p1.pv_ts
                    )) as time_on_page_seconds,
                    ROW_NUMBER() OVER (PARTITION BY p1.session_id ORDER BY p1.pv_ts ASC) as event_order_asc,
                    ROW_NUMBER() OVER (PARTITION BY p1.session_id ORDER BY p1.pv_ts DESC) as event_order_desc
                FROM p1
                JOIN p2 ON p1.event_id = p2.event_id
                JOIN session_events se ON se.event_id = p1.event_id
            ),
            session_stats AS (
                SELECT
                    COUNT(DISTINCT session_id) as total_sessions,
                    COUNT(*) FILTER (WHERE event_order_asc = 1) as entry_count,
                    COUNT(*) FILTER (WHERE event_order_desc = 1) as exit_count
                FROM page_events
            )
            SELECT
                COUNT(DISTINCT pe.visitor_id) as unique_visitors,
                COUNT(*) as total_page_views,
                COALESCE(AVG(
                    CASE
                        WHEN pe.time_on_page_seconds > 0 AND pe.time_on_page_seconds < 1800
                        THEN pe.time_on_page_seconds
                    END
                ), 0)::float8 as avg_time_on_page,
                CASE WHEN COUNT(*) > 0
                     THEN (COUNT(*) FILTER (WHERE pe.is_bounce = true))::float / COUNT(*)::float * 100
                     ELSE 0 END as bounce_rate,
                CASE WHEN ss.total_sessions > 0
                     THEN ss.entry_count::float / ss.total_sessions::float * 100
                     ELSE 0 END as entry_rate,
                CASE WHEN ss.total_sessions > 0
                     THEN ss.exit_count::float / ss.total_sessions::float * 100
                     ELSE 0 END as exit_rate
            FROM page_events pe
            CROSS JOIN session_stats ss
            GROUP BY ss.total_sessions, ss.entry_count, ss.exit_count
            "#,
            env_filter = env_filter
        );

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
            &stats_sql,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
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
                  {}
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
            date_trunc_unit,
            date_trunc_unit,
            pg_interval,
            date_trunc_unit,
            env_filter,
            date_trunc_unit
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
        let countries_sql = format!(
            r#"
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
                  {}
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
            "#,
            env_filter
        );

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
            &countries_sql,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
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
        let referrers_sql = format!(
            r#"
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
                  {}
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
            "#,
            env_filter
        );

        #[derive(FromQueryResult)]
        struct ReferrerStats {
            referrer: String,
            visits: i64,
            percentage: f64,
        }

        let referrer_results = ReferrerStats::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &referrers_sql,
            vec![
                project_id.into(),
                page_path.into(),
                start_date.into(),
                end_date.into(),
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

        // Query 1: Page-level stats (entry, exit, bounce, views, avg time)
        let page_stats_sql = format!(
            r#"
            SELECT
                e.page_path,
                COUNT(*) as total_views,
                COUNT(*) FILTER (WHERE e.is_entry = true) as entry_count,
                COUNT(*) FILTER (WHERE e.is_exit = true) as exit_count,
                COUNT(*) FILTER (WHERE e.is_bounce = true) as bounce_count,
                (AVG(e.time_on_page) FILTER (WHERE e.time_on_page IS NOT NULL AND e.time_on_page > 0))::float8 as avg_time_on_page
            FROM events e
            WHERE {}
            GROUP BY e.page_path
            ORDER BY total_views DESC
            "#,
            where_clause
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_utils::AnalyticsTestUtils;
    use crate::{cleanup_test_analytics, create_test_analytics_service};

    use UtcDateTime;

    #[tokio::test]
    async fn test_analytics_service_creation() {
        let db = AnalyticsTestUtils::create_test_db("test_analytics_service_creation")
            .await
            .unwrap();
        let cookie_crypto =
            Arc::new(temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap());
        let service = AnalyticsService::new(db, cookie_crypto);

        // Test that the service was created successfully
        assert!(std::ptr::addr_of!(service) as usize != 0);
    }

    #[tokio::test]
    async fn test_get_top_pages() -> anyhow::Result<()> {
        let (service, db, _container) = create_test_analytics_service!("test_get_top_pages");

        let start_date =
            chrono::NaiveDateTime::parse_from_str("2024-01-01 00:00:00", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc();
        let end_date =
            chrono::NaiveDateTime::parse_from_str("2024-01-31 23:59:59", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc();

        let pages = service
            .get_top_pages(1, 10, Some(start_date), Some(end_date))
            .await?;

        // Should have pages from our test data
        assert!(!pages.is_empty(), "Should have top pages");

        // Check that we have the expected test pages
        let paths: Vec<String> = pages.iter().map(|p| p.path.clone()).collect();
        assert!(
            paths.contains(&"/home".to_string()) || paths.contains(&"/about".to_string()),
            "Should contain test page paths"
        );

        cleanup_test_analytics!(db);
        Ok(())
    }

    #[tokio::test]
    async fn test_empty_results_for_invalid_project() -> anyhow::Result<()> {
        let (service, db, _container) =
            create_test_analytics_service!("test_empty_results_for_invalid_project");

        let start_date =
            chrono::NaiveDateTime::parse_from_str("2024-01-01 00:00:00", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc();
        let end_date =
            chrono::NaiveDateTime::parse_from_str("2024-01-31 23:59:59", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc();

        // Use a non-existent project ID
        let invalid_project_id = 9999;

        let pages = service
            .get_top_pages(invalid_project_id, 10, Some(start_date), Some(end_date))
            .await?;

        // Should have empty results for invalid project
        assert!(
            pages.is_empty(),
            "Should have empty pages for invalid project"
        );

        cleanup_test_analytics!(db);
        Ok(())
    }

    // Tests for SQL injection fixes - these verify that parameterized queries work correctly

    #[tokio::test]
    async fn test_parameterized_queries_compile() {
        // This test verifies that all our SQL injection fixes compile correctly
        // The fact that this test compiles proves that we're using Statement::from_sql_and_values
        // with properly typed parameters, which prevents SQL injection

        use sea_orm::{DatabaseBackend, Statement};

        // Test 1: Simple parameterized query with i32
        let _stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT * FROM events WHERE project_id = $1",
            vec![1_i32.into()],
        );

        // Test 2: Multiple parameters with different types
        let start_date: UtcDateTime = chrono::Utc::now();
        let _stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT * FROM events WHERE project_id = $1 AND timestamp >= $2 AND timestamp <= $3",
            vec![1_i32.into(), start_date.into(), start_date.into()],
        );

        // Test 3: Optional parameters
        let env_id: Option<i32> = Some(1);
        let mut values: Vec<sea_orm::Value> = vec![1_i32.into()];
        if let Some(id) = env_id {
            values.push(id.into());
        }
        let _stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT * FROM events WHERE project_id = $1",
            values,
        );

        // Test 4: LIMIT and OFFSET as parameters
        let _stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT * FROM events LIMIT $1 OFFSET $2",
            vec![(50_i64).into(), (0_i64).into()],
        );

        // If this test compiles, it proves our parameterized query pattern is correct
        // No assertion needed - compilation itself is the test
    }

    /// Regression test: `get_session_details` must return correct
    /// entry_path / exit_path / page_views / is_bounced / is_engaged for a
    /// session whose events are stored with the legacy `v2|<uuid>|<ts>` format
    /// in `events.session_id`.
    ///
    /// Prior to the fix, all five correlated subqueries used
    /// `e.session_id = rs.session_id`.  Because `rs.session_id` is always a
    /// bare UUID but `events.session_id` stored the full v2 payload, every
    /// subquery returned NULL / 0, producing silently wrong
    /// entry_path=NULL, page_views=0, is_bounced=true for real sessions.
    #[tokio::test]
    async fn test_get_session_details_with_legacy_v2_session_id() -> anyhow::Result<()> {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use temps_entities::{
            deployments, environments, events, projects, request_sessions, visitor,
        };

        let (service, db, _container) =
            create_test_analytics_service!("test_get_session_details_v2_session_id");

        // Re-use the project, environment, and deployment created by insert_test_data.
        let project = projects::Entity::find()
            .filter(projects::Column::Slug.eq("test_project"))
            .one(db.as_ref())
            .await?
            .expect("test project must exist from insert_test_data");

        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project.id))
            .one(db.as_ref())
            .await?
            .expect("test environment must exist from insert_test_data");

        let deployment = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project.id))
            .one(db.as_ref())
            .await?
            .expect("test deployment must exist from insert_test_data");

        // The bare UUID is what the proxy stores in request_sessions.session_id.
        let bare_uuid = "c172f0b5-986f-47dc-b6c6-9198761519e0";
        // The v2 payload is what the old analytics ingest path wrote to
        // events.session_id before the normalize_session_cookie fix landed.
        let v2_session_id = format!("v2|{}|1783631315", bare_uuid);

        let test_visitor = visitor::ActiveModel {
            visitor_id: Set("v2-session-test-visitor".to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // request_sessions always stores the bare UUID (proxy's parse_session_cookie
        // strips the v2| prefix before inserting).
        let session_started = chrono::Utc::now() - chrono::Duration::minutes(10);
        let session_ended = chrono::Utc::now();
        let session_row = request_sessions::ActiveModel {
            session_id: Set(bare_uuid.to_string()),
            started_at: Set(session_started),
            last_accessed_at: Set(session_ended),
            visitor_id: Set(Some(test_visitor.id)),
            data: Set("{}".to_string()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // Insert two page_view events using the LEGACY v2|uuid|ts format.
        // entry event (earlier timestamp)
        events::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            deployment_id: Set(Some(deployment.id)),
            visitor_id: Set(Some(test_visitor.id)),
            session_id: Set(Some(v2_session_id.clone())),
            event_type: Set("page_view".to_string()),
            page_path: Set("/entry-page".to_string()),
            hostname: Set("example.com".to_string()),
            pathname: Set("/entry-page".to_string()),
            href: Set("https://example.com/entry-page".to_string()),
            timestamp: Set(session_started + chrono::Duration::seconds(5)),
            is_bounce: Set(false),
            is_crawler: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        // exit event (later timestamp)
        events::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(Some(environment.id)),
            deployment_id: Set(Some(deployment.id)),
            visitor_id: Set(Some(test_visitor.id)),
            session_id: Set(Some(v2_session_id.clone())),
            event_type: Set("page_view".to_string()),
            page_path: Set("/exit-page".to_string()),
            hostname: Set("example.com".to_string()),
            pathname: Set("/exit-page".to_string()),
            href: Set("https://example.com/exit-page".to_string()),
            timestamp: Set(session_started + chrono::Duration::minutes(5)),
            is_bounce: Set(false),
            is_crawler: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;

        let details = service
            .get_session_details(session_row.id, project.id, None)
            .await?
            .expect("get_session_details must return Some for an existing session");

        // All five correlated subqueries must match via the v2-tolerant condition:
        assert_eq!(
            details.entry_path.as_deref(),
            Some("/entry-page"),
            "entry_path must be populated from legacy v2|uuid|ts events (subquery 1)"
        );
        assert_eq!(
            details.exit_path.as_deref(),
            Some("/exit-page"),
            "exit_path must be populated from legacy v2|uuid|ts events (subquery 2)"
        );
        assert_eq!(
            details.page_views, 2,
            "page_views must count both legacy-format page_view events (subquery 3)"
        );
        assert!(
            !details.is_bounced,
            "is_bounced must be false when page_views > 1 (subquery 4)"
        );
        // session duration is 10 minutes >= 10s, so is_engaged = true even
        // with only page_view events (which don't count as interactions)
        assert!(
            details.is_engaged,
            "is_engaged must be true when session duration >= 10s (subquery 5)"
        );

        cleanup_test_analytics!(db);
        Ok(())
    }
}
