// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Instance analytics: bounded aggregate results, never one query per project.
#[derive(Debug, thiserror::Error)]
pub enum GlobalAnalyticsError {
    #[error("Invalid global analytics query: {reason}")]
    InvalidQuery { reason: String },
    #[error("Could not read aggregate analytics from PostgreSQL: {0}")]
    Database(#[from] sea_orm::DbErr),
}
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseBackend, DatabaseConnection, FromQueryResult, Statement, Value};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

#[derive(Clone, Copy, Debug, Default, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsFacet {
    #[default]
    Summary,
    Traffic,
    Pages,
    Events,
    Breakdown,
    Speed,
}
#[derive(Debug, Deserialize, IntoParams)]
pub struct GlobalAnalyticsQuery {
    pub facet: Option<AnalyticsFacet>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    #[param(value_type = String, format = DateTime)]
    pub start_date: DateTime<Utc>,
    #[param(value_type = String, format = DateTime)]
    pub end_date: DateTime<Utc>,
    pub search: Option<String>,
    pub dimension: Option<String>,
    pub device: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub min_views: Option<u32>,
    pub min_sessions: Option<u32>,
}
#[derive(Debug, Serialize, ToSchema, FromQueryResult)]
pub struct GlobalAnalyticsRow {
    pub key: String,
    pub project_id: i32,
    pub project_name: String,
    pub views: i64,
    pub sessions: i64,
    pub visitors: i64,
    pub avg_time_seconds: Option<f64>,
    pub bounce_rate: Option<f64>,
    pub lcp_p75: Option<f64>,
    pub inp_p75: Option<f64>,
    pub cls_p75: Option<f64>,
    pub ttfb_p75: Option<f64>,
    pub fcp_p75: Option<f64>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalAnalyticsResponse {
    pub rows: Vec<GlobalAnalyticsRow>,
    pub total: i64,
    pub total_views: i64,
}
#[derive(Debug, Serialize, ToSchema, FromQueryResult)]
pub struct AnalyticsProjectOption {
    pub project_id: i32,
    pub project_name: String,
}

pub struct GlobalAnalyticsService {
    db: Arc<DatabaseConnection>,
}
impl GlobalAnalyticsService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
    pub async fn projects(
        &self,
        project: Option<i32>,
        hidden: &[i32],
    ) -> Result<Vec<AnalyticsProjectOption>, GlobalAnalyticsError> {
        Ok(AnalyticsProjectOption::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT id AS project_id, name AS project_name FROM projects WHERE NOT is_deleted AND ($1::int IS NULL OR id=$1) AND NOT (id=ANY($2::int[])) ORDER BY name,id",
            vec![project.into(), hidden.to_vec().into()])).all(self.db.as_ref()).await?)
    }
    pub async fn query(
        &self,
        q: &GlobalAnalyticsQuery,
        hidden: &[i32],
    ) -> Result<GlobalAnalyticsResponse, GlobalAnalyticsError> {
        let (cte, mut values, order) = build_query(q, hidden)?;
        #[derive(FromQueryResult)]
        struct Total {
            total: i64,
            total_views: i64,
        }
        let total = Total::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            format!("{cte} SELECT count(*)::bigint AS total, COALESCE(sum(views),0)::bigint AS total_views FROM filtered"), values.clone()))
            .one(self.db.as_ref()).await?.map_or((0,0), |v| (v.total,v.total_views));
        let facet = q.facet.unwrap_or_default();
        let size = if facet == AnalyticsFacet::Traffic {
            2161
        } else {
            q.per_page.unwrap_or(20) as i64
        };
        let offset = if facet == AnalyticsFacet::Traffic {
            0
        } else {
            (q.page.unwrap_or(1) as i64 - 1) * size
        };
        let n = values.len();
        values.push(size.into());
        values.push(offset.into());
        let rows = GlobalAnalyticsRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "{cte} SELECT * FROM filtered ORDER BY {order} LIMIT ${} OFFSET ${}",
                n + 1,
                n + 2
            ),
            values,
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(GlobalAnalyticsResponse {
            rows,
            total: total.0,
            total_views: total.1,
        })
    }
}
fn invalid(message: &str) -> GlobalAnalyticsError {
    GlobalAnalyticsError::InvalidQuery {
        reason: message.to_owned(),
    }
}

pub fn validate(q: &GlobalAnalyticsQuery) -> Result<(), GlobalAnalyticsError> {
    if q.start_date >= q.end_date || q.end_date - q.start_date > chrono::Duration::days(90) {
        return Err(invalid("time window must be between 0 and 90 days"));
    }
    if !(1..=100).contains(&q.per_page.unwrap_or(20)) || q.page == Some(0) {
        return Err(invalid(
            "page must be positive and per_page between 1 and 100",
        ));
    }
    if q.search.as_ref().is_some_and(|s| s.len() > 500) {
        return Err(invalid("search exceeds 500 bytes"));
    }
    if q.project_id.is_some_and(|id| id <= 0)
        || q.environment_id.is_some_and(|id| id <= 0)
        || (q.environment_id.is_some() && q.project_id.is_none())
    {
        return Err(invalid("environment requires a positive project scope"));
    }
    if q.device
        .as_deref()
        .is_some_and(|d| !["desktop", "mobile", "tablet"].contains(&d))
    {
        return Err(invalid("unknown device"));
    }
    Ok(())
}
fn build_query(
    q: &GlobalAnalyticsQuery,
    hidden: &[i32],
) -> Result<(String, Vec<Value>, String), GlobalAnalyticsError> {
    validate(q)?;
    let facet = q.facet.unwrap_or_default();
    let dimension = match q.dimension.as_deref().unwrap_or("country") {
        "country" => "g.country",
        "region" => "g.region",
        "city" => "g.city",
        "language" => "e.language",
        "browser" => "e.browser",
        "device_type" => "e.device_type",
        "operating_system" => "e.operating_system",
        "referrer_hostname" => "e.referrer_hostname",
        "utm_campaign" => "e.utm_campaign",
        "utm_source" => "e.utm_source",
        "utm_medium" => "e.utm_medium",
        "utm_term" => "e.utm_term",
        "utm_content" => "e.utm_content",
        "channel" => "e.channel",
        _ => return Err(invalid("unknown breakdown dimension")),
    };
    let key=match facet {
        AnalyticsFacet::Pages=>"e.page_path".to_string(),
        AnalyticsFacet::Events=>"COALESCE(e.event_name,e.event_type)".to_string(),
        AnalyticsFacet::Traffic=>"to_char(date_trunc('hour',e.timestamp AT TIME ZONE 'UTC'), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')".to_string(),
        AnalyticsFacet::Breakdown=>format!("COALESCE({dimension},'Unknown')"),
        _=>"''::text".to_string(),
    };
    let grouped = matches!(
        facet,
        AnalyticsFacet::Pages | AnalyticsFacet::Events | AnalyticsFacet::Speed
    );
    let identity = if grouped {
        "e.project_id,p.name"
    } else {
        "0::int AS project_id,''::text AS name"
    };
    let group = if grouped {
        "GROUP BY 1,2,3"
    } else if facet == AnalyticsFacet::Summary {
        ""
    } else {
        "GROUP BY 1"
    };
    let event_filter=match facet {
        AnalyticsFacet::Events=>"COALESCE(e.event_name,e.event_type) NOT IN ('page_view','page_leave','heartbeat') AND e.event_name IS NOT NULL",
        AnalyticsFacet::Speed=>"COALESCE(g.is_hosting_provider, false) = false AND (e.lcp IS NOT NULL OR e.inp IS NOT NULL OR e.cls IS NOT NULL OR e.ttfb IS NOT NULL OR e.fcp IS NOT NULL)",
        AnalyticsFacet::Pages=>"e.event_type='page_view' AND e.page_path<>''",
        _=>"e.event_type='page_view'",
    };
    let vitals = if facet == AnalyticsFacet::Speed {
        ["lcp", "inp", "cls", "ttfb", "fcp"]
            .map(|k| {
                format!("percentile_cont(0.75) WITHIN GROUP (ORDER BY e.{k})::float8 AS {k}_p75")
            })
            .join(",")
    } else {
        "NULL::float8 AS lcp_p75,NULL::float8 AS inp_p75,NULL::float8 AS cls_p75,NULL::float8 AS ttfb_p75,NULL::float8 AS fcp_p75".into()
    };
    let sort = match q.sort_by.as_deref().unwrap_or("views") {
        "views" => "views",
        "sessions" => "sessions",
        "visitors" => "visitors",
        "key" => "key",
        "project" => "project_name",
        "avg_time_seconds" => "avg_time_seconds",
        "lcp_p75" => "lcp_p75",
        "inp_p75" => "inp_p75",
        "cls_p75" => "cls_p75",
        "ttfb_p75" => "ttfb_p75",
        "fcp_p75" => "fcp_p75",
        _ => return Err(invalid("unknown sort field")),
    };
    let dir = match q.sort_order.as_deref().unwrap_or("desc") {
        "asc" => "ASC",
        "desc" => "DESC",
        _ => return Err(invalid("unknown sort direction")),
    };
    let order = if facet == AnalyticsFacet::Traffic {
        "key ASC".into()
    } else {
        format!("{sort} {dir} NULLS LAST,project_id ASC,key ASC")
    };
    // All filters are bound; only allowlisted identifiers enter SQL. Aggregate
    // before LIMIT so ranking/search remain correct beyond any project's top N.
    let source = match facet {
        AnalyticsFacet::Summary => "(SELECT e.*,count(*) OVER (PARTITION BY project_id,environment_id,CASE WHEN session_id LIKE 'v2|%' THEN split_part(session_id,'|',2) ELSE session_id END) AS session_views FROM events e
            WHERE timestamp >= $1 AND timestamp < $2 AND NOT is_crawler AND event_type='page_view'
            AND ($3::int IS NULL OR project_id=$3) AND ($4::int IS NULL OR environment_id=$4) AND NOT (project_id=ANY($5::int[])))",

        AnalyticsFacet::Speed => "(SELECT pm.*,pm.recorded_at AS timestamp,NULL::int AS time_on_page,false AS is_bounce FROM performance_metrics pm)",
        AnalyticsFacet::Pages => "(SELECT timed.*,COALESCE(time_on_page::float8, EXTRACT(EPOCH FROM (COALESCE(CASE WHEN next_leave <= timestamp + INTERVAL '30 minutes' THEN next_leave END,next_view,timestamp+INTERVAL '30 seconds')-timestamp))) AS duration FROM (
            SELECT e.*,
            min(timestamp) FILTER (WHERE event_type='page_leave') OVER (PARTITION BY project_id,environment_id,COALESCE(CASE WHEN session_id LIKE 'v2|%' THEN split_part(session_id,'|',2) ELSE session_id END,'event:'||id::text),page_path ORDER BY timestamp,id ROWS BETWEEN 1 FOLLOWING AND UNBOUNDED FOLLOWING) AS next_leave,
            min(timestamp) FILTER (WHERE event_type='page_view') OVER (PARTITION BY project_id,environment_id,COALESCE(CASE WHEN session_id LIKE 'v2|%' THEN split_part(session_id,'|',2) ELSE session_id END,'event:'||id::text) ORDER BY timestamp,id ROWS BETWEEN 1 FOLLOWING AND UNBOUNDED FOLLOWING) AS next_view
            FROM events e WHERE timestamp >= $1 AND timestamp < $2::timestamptz + INTERVAL '30 minutes'
            AND NOT is_crawler AND event_type IN ('page_view','page_leave') AND ($3::int IS NULL OR project_id=$3) AND ($4::int IS NULL OR environment_id=$4) AND NOT (project_id=ANY($5::int[]))
        ) timed)",
        _ => "events",
    };
    let session = if facet == AnalyticsFacet::Speed {
        "e.session_id"
    } else {
        "CASE WHEN e.session_id LIKE 'v2|%' THEN split_part(e.session_id,'|',2) ELSE e.session_id END"
    };
    let bounced = if facet == AnalyticsFacet::Summary {
        "e.session_views=1"
    } else {
        "e.is_bounce"
    };
    let duration = if facet == AnalyticsFacet::Pages {
        "e.duration"
    } else {
        "e.time_on_page"
    };
    // Match project performance views: normal browser UAs from hosting-provider
    // IPs are excluded before counts and percentiles, while unknown IPs remain.
    let geo_join = if facet == AnalyticsFacet::Speed {
        "LEFT JOIN ip_geolocations g ON g.id=e.ip_address_id"
    } else if facet == AnalyticsFacet::Breakdown
        && matches!(
            q.dimension.as_deref().unwrap_or("country"),
            "country" | "region" | "city"
        )
    {
        "LEFT JOIN ip_geolocations g ON g.id=e.ip_geolocation_id"
    } else {
        ""
    };
    let sql=format!("WITH grouped AS (
        SELECT {key} AS key,{identity},count(*)::bigint AS views,
        count(DISTINCT (e.project_id,e.environment_id,{session})) FILTER (WHERE e.session_id IS NOT NULL)::bigint AS sessions,
        count(DISTINCT (e.project_id,e.visitor_id)) FILTER (WHERE e.visitor_id IS NOT NULL)::bigint AS visitors,
        avg({duration}) FILTER (WHERE {duration}>0 AND {duration}<1800)::float8 AS avg_time_seconds,
        100.0*count(DISTINCT (e.project_id,e.environment_id,{session})) FILTER (WHERE {bounced} AND e.session_id IS NOT NULL)/NULLIF(count(DISTINCT (e.project_id,e.environment_id,{session})) FILTER (WHERE e.session_id IS NOT NULL),0)::float8 AS bounce_rate,
        {vitals}
        FROM {source} e JOIN projects p ON p.id=e.project_id AND NOT p.is_deleted
        {geo_join}
        WHERE e.timestamp >= $1 AND e.timestamp < $2 AND NOT e.is_crawler
          AND ($3::int IS NULL OR e.project_id=$3) AND ($4::int IS NULL OR e.environment_id=$4)
          AND NOT (e.project_id=ANY($5::int[])) AND ($6::text IS NULL OR e.device_type=$6 OR ($6='desktop' AND e.device_type='pc') OR ($6='mobile' AND e.device_type IN ('smartphone','mobilephone')))
          AND {event_filter} {group}
    ), filtered AS (SELECT key,project_id,name AS project_name,views,sessions,visitors,avg_time_seconds,bounce_rate,lcp_p75,inp_p75,cls_p75,ttfb_p75,fcp_p75 FROM grouped
       WHERE ($7::text='' OR strpos(lower(key),lower($7))>0 OR strpos(lower(name),lower($7))>0)
         AND views >= $8 AND sessions >= $9)");
    Ok((
        sql,
        vec![
            q.start_date.into(),
            q.end_date.into(),
            q.project_id.into(),
            q.environment_id.into(),
            hidden.to_vec().into(),
            q.device.clone().into(),
            q.search.clone().unwrap_or_default().into(),
            (q.min_views.unwrap_or(0) as i64).into(),
            (q.min_sessions.unwrap_or(0) as i64).into(),
        ],
        order,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database};
    fn query() -> GlobalAnalyticsQuery {
        serde_json::from_value(serde_json::json!({"facet":"pages","start_date":"2026-01-01T00:00:00Z","end_date":"2026-01-02T00:00:00Z","per_page":1})).unwrap()
    }
    #[test]
    fn rejects_unbounded_and_injected_queries() {
        let mut q = query();
        q.per_page = Some(101);
        assert!(build_query(&q, &[]).is_err());
        q = query();
        q.sort_by = Some("views; DROP TABLE events".into());
        assert!(build_query(&q, &[]).is_err());
        q = query();
        q.dimension = Some("untrusted".into());
        assert!(build_query(&q, &[]).is_err());
        q = query();
        q.environment_id = Some(1);
        assert!(validate(&q).is_err());
        q = query();
        q.search = Some("%' OR true --".into());
        let (sql, values, _) = build_query(&q, &[9]).unwrap();
        assert!(!sql.contains("OR true"));
        assert_eq!(values.len(), 9);
    }
    #[tokio::test]
    async fn global_speed_excludes_hosting_providers_before_counts_and_percentiles() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("Skipping PostgreSQL integration: TEST_DATABASE_URL is not configured");
            return;
        };
        let mut options = ConnectOptions::new(url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        db.execute_unprepared(r#"
            CREATE TEMP TABLE projects(id int, name text, is_deleted bool DEFAULT false);
            CREATE TEMP TABLE ip_geolocations(id int, is_hosting_provider bool);
            CREATE TEMP TABLE performance_metrics(project_id int, environment_id int, recorded_at timestamptz,
                is_crawler bool, ip_address_id int, session_id int, visitor_id int, device_type text,
                lcp real, inp real, cls real, ttfb real, fcp real);
            INSERT INTO projects VALUES(1,'First app',false),(2,'Second app',false);
            INSERT INTO ip_geolocations VALUES(1,true),(2,false),(3,NULL);
            INSERT INTO performance_metrics
                SELECT 1,10,'2026-01-01T01:00:00Z',false,ip,ip,ip,'desktop',v,v,v,v,v
                FROM (VALUES (2,100),(3,200),(NULL::int,300)) AS sample(ip,v);
            INSERT INTO performance_metrics
                SELECT 1,10,'2026-01-01T01:00:00Z',false,1,n,n,'desktop',9999,9999,9999,9999,9999
                FROM generate_series(10,109) n;
            INSERT INTO performance_metrics VALUES
                (1,10,'2026-01-01T01:00:00Z',true,2,200,200,'desktop',9999,9999,9999,9999,9999),
                (2,20,'2026-01-01T01:00:00Z',false,2,1,1,'desktop',500,500,500,500,500);
        "#).await.unwrap();
        let service = GlobalAnalyticsService::new(Arc::new(db));
        let mut q = query();
        q.facet = Some(AnalyticsFacet::Speed);
        let result = service.query(&q, &[2]).await.unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.total_views, 3);
        let row = &result.rows[0];
        assert_eq!(
            row.views, 3,
            "hosting-provider and crawler samples must not affect counts"
        );
        for percentile in [
            row.lcp_p75,
            row.inp_p75,
            row.cls_p75,
            row.ttfb_p75,
            row.fcp_p75,
        ] {
            assert_eq!(
                percentile,
                Some(250.0),
                "unknown/missing geolocation must remain included"
            );
        }
        q.project_id = Some(1);
        q.environment_id = Some(10);
        assert_eq!(service.query(&q, &[]).await.unwrap().total_views, 3);
        q.environment_id = Some(20);
        assert_eq!(service.query(&q, &[]).await.unwrap().total, 0);
    }

    // A dedicated single-connection pool owns only TEMP tables. No shared
    // database objects or operator data are modified, even on a developer DB.
    #[tokio::test]
    async fn global_ranking_filtering_and_scope() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("Skipping PostgreSQL integration: TEST_DATABASE_URL is not configured");
            return;
        };
        let mut options = ConnectOptions::new(url);
        options.max_connections(1).min_connections(1);
        let db = Database::connect(options).await.unwrap();
        db.execute_unprepared("CREATE TEMP TABLE projects(id int,name text,is_deleted bool DEFAULT false);
          CREATE TEMP TABLE ip_geolocations(id int,country text,region text,city text,is_hosting_provider bool);
          CREATE TEMP TABLE events(id bigserial,project_id int, environment_id int, timestamp timestamptz, is_crawler bool DEFAULT false,
          event_type text DEFAULT 'page_view',event_name text,page_path text,session_id text,visitor_id int,time_on_page int,is_bounce bool DEFAULT false,
          device_type text,ip_geolocation_id int,lcp real,inp real,cls real,ttfb real,fcp real);
          INSERT INTO projects(id,name) VALUES (1,'First app'),(2,'Second app'),(3,'Hidden app');
          INSERT INTO events(project_id,environment_id,timestamp,page_path,session_id,visitor_id,time_on_page,lcp,device_type)
          SELECT 1,10,'2026-01-01T01:00:00Z','/popular','repeat',1,20,100,'desktop' FROM generate_series(1,10);
          INSERT INTO events(project_id,environment_id,timestamp,page_path,session_id,visitor_id,time_on_page,lcp,device_type)
          SELECT 2,20,'2026-01-01T01:00:00Z','/popular','session-'||n,n,40,300,'desktop' FROM generate_series(1,5) n;
          INSERT INTO events(project_id,environment_id,timestamp,page_path,session_id,visitor_id)
          SELECT 3,30,'2026-01-01T01:00:00Z','/hidden','hidden-'||n,n FROM generate_series(1,100) n;
          CREATE TEMP TABLE performance_metrics AS SELECT ip_geolocation_id AS ip_address_id,project_id,environment_id,timestamp AS recorded_at,is_crawler,session_id,visitor_id,lcp,inp,cls,ttfb,fcp,device_type FROM events;
          INSERT INTO events(project_id,environment_id,timestamp,page_path,session_id,is_crawler) VALUES (1,11,'2026-01-01T01:00:00Z','/preview','preview',false),(2,20,'2026-01-01T01:00:00Z','/bot','bot',true);").await.unwrap();
        let service = GlobalAnalyticsService::new(Arc::new(db));
        let mut q = query();
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.total, 3);
        assert_eq!(r.rows[0].project_id, 1);
        assert_eq!(r.rows[0].views, 10);
        q.sort_by = Some("sessions".into());
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.rows[0].project_id, 2);
        assert_eq!(r.rows[0].sessions, 5);
        q.page = Some(2);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.rows[0].project_id, 1);
        q.search = Some("SECOND".into());
        q.page = Some(1);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.rows[0].project_id, 2);
        q.search = Some("absent".into());
        q.page = Some(500);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.total, 0);
        assert!(r.rows.is_empty());
        q = query();
        q.project_id = Some(1);
        q.environment_id = Some(11);
        let r = service.query(&q, &[]).await.unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.rows[0].key, "/preview");
        q = query();
        q.min_sessions = Some(2);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.rows[0].project_id, 2);
        q = query();
        q.facet = Some(AnalyticsFacet::Summary);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.rows[0].views, 16);
        assert_eq!(r.rows[0].sessions, 7);
        assert!((r.rows[0].bounce_rate.unwrap() - 600.0 / 7.0).abs() < 0.001);
        q.facet = Some(AnalyticsFacet::Traffic);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.rows[0].key, "2026-01-01T01:00:00Z");
        assert_eq!(r.rows[0].visitors, 6);
        q.facet = Some(AnalyticsFacet::Speed);
        q.per_page = Some(20);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0].lcp_p75, Some(100.0));
        assert_eq!(r.rows[1].lcp_p75, Some(300.0));
        q.facet = Some(AnalyticsFacet::Breakdown);
        let r = service.query(&q, &[3]).await.unwrap();
        assert_eq!(r.rows[0].views, 16);
        assert_eq!(service.projects(None, &[3]).await.unwrap().len(), 2);
        assert!(service.projects(Some(3), &[3]).await.unwrap().is_empty());
    }
}
