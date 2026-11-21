// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SQL for conversion goals: which `events` rows a goal matches, and the two
//! reports built on that (per-goal totals, one goal broken down by a
//! dimension).
//!
//! Every value from a goal or a request is bound as a parameter. The only
//! things formatted into the SQL text are column names taken from closed
//! enums ([`SmartFilter::to_condition`], [`AnalyticsGoalBreakdownDimension`]), JSON
//! keys that [`validate_json_key`] restricted to `[A-Za-z0-9_]`, and goal ids,
//! which are integers.

use chrono::{DateTime, Utc};
use sea_orm::Value;

use super::types::{AnalyticsGoal, AnalyticsGoalBreakdownDimension, AnalyticsGoalKind};
use crate::services::SmartFilter;

/// Positional parameters of one statement.
#[derive(Debug, Default)]
pub(crate) struct Params {
    pub values: Vec<Value>,
}

impl Params {
    /// Bind `value` and return its placeholder.
    pub fn push(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }
}

/// True for a key that may be written into the SQL text as a JSON key.
pub(crate) fn validate_json_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Translate a page pattern to a `LIKE` pattern: `*` matches any run of
/// characters, and `%`, `_` and `\` are matched literally.
pub(crate) fn page_like_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    for c in pattern.chars() {
        match c {
            '*' => out.push('%'),
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// Rows that can be a conversion: people, plus the UA-less events a backend
/// sends through the authenticated ingest route, which are stored as the bot
/// `unknown` (a purchase recorded server-side is still a purchase). Detected
/// bots and traffic a project filter excluded never convert; an excluded
/// event carries its filter reason as `crawler_name`, so it is never
/// `unknown`.
pub(crate) const CONVERSION_TRAFFIC: &str = "(e.is_crawler = false OR e.crawler_name = 'unknown')";

/// Rows that count as a visitor in a conversion-rate denominator: people's
/// page views, as the overview counts visitors.
pub(crate) const VISITOR_TRAFFIC: &str = "e.is_crawler = false AND e.event_type = 'page_view'";

/// Rows the reports consider: one project, one period. Callers add
/// [`CONVERSION_TRAFFIC`] or [`VISITOR_TRAFFIC`].
pub(crate) fn scope_condition(
    params: &mut Params,
    project_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    environment_id: Option<i32>,
) -> String {
    let mut sql = format!(
        "e.project_id = {} AND e.timestamp >= {} AND e.timestamp < {}",
        params.push(project_id),
        params.push(start),
        params.push(end),
    );
    if let Some(environment_id) = environment_id {
        sql.push_str(&format!(
            " AND e.environment_id = {}",
            params.push(environment_id)
        ));
    }
    sql
}

/// JSON text of a nested custom property, reading `props` and then the legacy
/// `event_data`, as funnel steps do.
fn json_text_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('.').collect();
    if !parts.iter().all(|part| validate_json_key(part)) {
        return None;
    }
    let mut sql = "COALESCE(e.props, e.event_data::jsonb, '{}'::jsonb)".to_string();
    for (i, part) in parts.iter().enumerate() {
        let op = if i + 1 == parts.len() { "->>" } else { "->" };
        sql.push_str(&format!("{op}'{part}'"));
    }
    Some(sql)
}

/// Condition an `events e` row meets when it is a conversion of `goal`.
/// `None` when the goal holds something validation would have refused, so a
/// row written by hand never turns into unsafe SQL.
pub(crate) fn goal_condition(goal: &AnalyticsGoal, params: &mut Params) -> Option<String> {
    let mut parts = Vec::new();
    match goal.kind {
        AnalyticsGoalKind::Event => {
            let name = goal.event_name.as_ref()?;
            parts.push(format!(
                "COALESCE(e.event_name, e.event_type) = {}",
                params.push(name.clone())
            ));
        }
        AnalyticsGoalKind::Page => {
            let path = goal.page_path.as_ref()?;
            parts.push("e.event_type = 'page_view'".to_string());
            if path.contains('*') {
                parts.push(format!(
                    "e.pathname LIKE {} ESCAPE '\\'",
                    params.push(page_like_pattern(path))
                ));
            } else {
                parts.push(format!("e.pathname = {}", params.push(path.clone())));
            }
        }
    }
    for filter in &goal.event_filter {
        match filter {
            SmartFilter::CustomData { path, value } => {
                let column = json_text_path(path)?;
                parts.push(format!("{column} = {}", params.push(value.clone())));
            }
            other => {
                let (column, value) = other.to_condition()?;
                parts.push(format!("e.{column} = {}", params.push(value)));
            }
        }
    }
    Some(parts.join(" AND "))
}

/// Numeric revenue of a row, `NULL` when the property is missing or is not a
/// plain decimal number of at most 15 integer and 6 decimal digits, so one
/// malformed or absurdly large value (which would overflow the `float8` sum)
/// cannot fail the report.
pub(crate) fn revenue_expression(goal: &AnalyticsGoal, params: &mut Params) -> String {
    match &goal.revenue_property {
        Some(property) => {
            let key = params.push(property.clone());
            format!(
                "CASE WHEN (e.props ->> {key}::text) ~ '^-?[0-9]{{1,15}}(\\.[0-9]{{1,6}})?$' \
                 THEN (e.props ->> {key}::text)::numeric END"
            )
        }
        None => "NULL::numeric".to_string(),
    }
}

/// Totals of every goal in one scan of `events`: each row joins the goals it
/// matches through a lateral `UNION ALL`, then rows are grouped by goal.
/// `None` when there are no usable goals.
pub(crate) fn stats_query(
    goals: &[AnalyticsGoal],
    project_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    environment_id: Option<i32>,
) -> Option<(String, Vec<Value>)> {
    let mut params = Params::default();
    let scope = scope_condition(&mut params, project_id, start, end, environment_id);
    let branches: Vec<String> = goals
        .iter()
        .filter_map(|goal| {
            let condition = goal_condition(goal, &mut params)?;
            let revenue = revenue_expression(goal, &mut params);
            Some(format!(
                "SELECT {}::int AS goal_id, {revenue} AS revenue WHERE {condition}",
                goal.id
            ))
        })
        .collect();
    if branches.is_empty() {
        return None;
    }
    let sql = format!(
        "SELECT g.goal_id, \
                COUNT(*)::bigint AS total_conversions, \
                COUNT(DISTINCT e.visitor_id)::bigint AS unique_conversions, \
                SUM(g.revenue)::float8 AS revenue_total, \
                COUNT(g.revenue)::bigint AS revenue_count \
         FROM events e \
         CROSS JOIN LATERAL ({}) g \
         WHERE {scope} AND {CONVERSION_TRAFFIC} \
         GROUP BY g.goal_id",
        branches.join(" UNION ALL ")
    );
    Some((sql, params.values))
}

/// Distinct visitors with a page view in the period: the denominator of a
/// conversion rate, counted as the overview counts visitors.
pub(crate) fn visitors_query(
    project_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    environment_id: Option<i32>,
) -> (String, Vec<Value>) {
    let mut params = Params::default();
    let scope = scope_condition(&mut params, project_id, start, end, environment_id);
    (
        format!(
            "SELECT COUNT(DISTINCT e.visitor_id)::bigint AS visitors \
             FROM events e WHERE {scope} AND {VISITOR_TRAFFIC}"
        ),
        params.values,
    )
}

/// Expression and join a breakdown groups by. Source dimensions are the
/// visitor's first touch, as the visitors list's segment filters are.
fn dimension_sql(dimension: AnalyticsGoalBreakdownDimension) -> (&'static str, &'static str) {
    const VISITOR: &str = "LEFT JOIN visitor v ON v.id = e.visitor_id";
    const GEO: &str = "LEFT JOIN ip_geolocations geo ON geo.id = e.ip_geolocation_id";
    match dimension {
        AnalyticsGoalBreakdownDimension::Channel => ("v.first_channel", VISITOR),
        AnalyticsGoalBreakdownDimension::Referrer => ("v.first_referrer_hostname", VISITOR),
        AnalyticsGoalBreakdownDimension::UtmSource => ("v.first_utm_source", VISITOR),
        AnalyticsGoalBreakdownDimension::UtmMedium => ("v.first_utm_medium", VISITOR),
        AnalyticsGoalBreakdownDimension::UtmCampaign => ("v.first_utm_campaign", VISITOR),
        AnalyticsGoalBreakdownDimension::Page => ("e.pathname", ""),
        AnalyticsGoalBreakdownDimension::Country => ("geo.country", GEO),
        AnalyticsGoalBreakdownDimension::DeviceType => ("e.device_type", ""),
        AnalyticsGoalBreakdownDimension::Browser => ("e.browser", ""),
        AnalyticsGoalBreakdownDimension::OperatingSystem => ("e.operating_system", ""),
    }
}

/// One goal's conversions grouped by `dimension`, with the visitors of each
/// value as the conversion-rate denominator. `None` when the stored goal is
/// unusable.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one independent part of the report scope; a struct would only be unpacked again here"
)]
pub(crate) fn breakdown_query(
    goal: &AnalyticsGoal,
    dimension: AnalyticsGoalBreakdownDimension,
    project_id: i32,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    environment_id: Option<i32>,
    limit: u64,
) -> Option<(String, Vec<Value>)> {
    let mut params = Params::default();
    let scope = scope_condition(&mut params, project_id, start, end, environment_id);
    let condition = goal_condition(goal, &mut params)?;
    let revenue = revenue_expression(goal, &mut params);
    let limit = params.push(limit as i64);
    let (value, join) = dimension_sql(dimension);
    let sql = format!(
        "WITH conv AS ( \
            SELECT CAST({value} AS TEXT) AS value, \
                   COUNT(*)::bigint AS total_conversions, \
                   COUNT(DISTINCT e.visitor_id)::bigint AS unique_conversions, \
                   SUM({revenue})::float8 AS revenue_total, \
                   COUNT({revenue})::bigint AS revenue_count \
            FROM events e {join} \
            WHERE {scope} AND {CONVERSION_TRAFFIC} AND {condition} \
            GROUP BY 1 \
         ), vis AS ( \
            SELECT CAST({value} AS TEXT) AS value, \
                   COUNT(DISTINCT e.visitor_id)::bigint AS visitors \
            FROM events e {join} \
            WHERE {scope} AND {VISITOR_TRAFFIC} \
            GROUP BY 1 \
         ) \
         SELECT conv.value, COALESCE(vis.visitors, 0)::bigint AS visitors, \
                conv.unique_conversions, conv.total_conversions, \
                conv.revenue_total, conv.revenue_count \
         FROM conv LEFT JOIN vis ON vis.value IS NOT DISTINCT FROM conv.value \
         ORDER BY conv.unique_conversions DESC, conv.total_conversions DESC, \
                  conv.value ASC NULLS LAST \
         LIMIT {limit}"
    );
    Some((sql, params.values))
}

/// `part` as a percentage of `whole`, 0 for an empty whole, never above 100
/// (a visitor can convert without a page view in the period).
pub(crate) fn percentage(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        return 0.0;
    }
    (part as f64 / whole as f64 * 100.0).min(100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(kind: AnalyticsGoalKind) -> AnalyticsGoal {
        AnalyticsGoal {
            id: 7,
            project_id: 1,
            name: "Signup".into(),
            kind,
            event_name: (kind == AnalyticsGoalKind::Event).then(|| "signup".into()),
            event_filter: vec![],
            page_path: (kind == AnalyticsGoalKind::Page).then(|| "/thank-you".into()),
            revenue_property: None,
            revenue_currency: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn page_patterns_treat_only_the_star_as_a_wildcard() {
        assert_eq!(page_like_pattern("/docs/*"), "/docs/%");
        assert_eq!(page_like_pattern("/100%_off"), "/100\\%\\_off");
        assert_eq!(page_like_pattern("/a\\b"), "/a\\\\b");
    }

    #[test]
    fn an_event_goal_matches_by_name_and_binds_every_filter() {
        let mut g = goal(AnalyticsGoalKind::Event);
        g.event_filter = vec![
            SmartFilter::UtmSource("newsletter".into()),
            SmartFilter::CustomData {
                path: "plan.tier".into(),
                value: "pro".into(),
            },
        ];
        let mut params = Params::default();
        let sql = goal_condition(&g, &mut params).expect("valid goal");
        assert_eq!(
            sql,
            "COALESCE(e.event_name, e.event_type) = $1 AND e.utm_source = $2 AND \
             COALESCE(e.props, e.event_data::jsonb, '{}'::jsonb)->'plan'->>'tier' = $3"
        );
        assert_eq!(params.values.len(), 3);
    }

    #[test]
    fn a_page_goal_uses_equality_without_a_wildcard_and_like_with_one() {
        let mut params = Params::default();
        let exact =
            goal_condition(&goal(AnalyticsGoalKind::Page), &mut params).expect("valid goal");
        assert_eq!(exact, "e.event_type = 'page_view' AND e.pathname = $1");

        let mut g = goal(AnalyticsGoalKind::Page);
        g.page_path = Some("/docs/*".into());
        let mut params = Params::default();
        let like = goal_condition(&g, &mut params).expect("valid goal");
        assert_eq!(
            like,
            "e.event_type = 'page_view' AND e.pathname LIKE $1 ESCAPE '\\'"
        );
        assert_eq!(params.values, vec![Value::from("/docs/%")]);
    }

    #[test]
    fn a_stored_filter_with_an_unsafe_json_key_never_reaches_the_sql() {
        let mut g = goal(AnalyticsGoalKind::Event);
        g.event_filter = vec![SmartFilter::CustomData {
            path: "plan'; DROP TABLE events; --".into(),
            value: "x".into(),
        }];
        assert!(goal_condition(&g, &mut Params::default()).is_none());
    }

    #[test]
    fn revenue_is_read_only_from_plain_decimal_values() {
        let mut g = goal(AnalyticsGoalKind::Event);
        g.revenue_property = Some("amount".into());
        g.revenue_currency = Some("EUR".into());
        let mut params = Params::default();
        let sql = revenue_expression(&g, &mut params);
        assert!(sql.contains("(e.props ->> $1::text) ~ '^-?[0-9]{1,15}(\\.[0-9]{1,6})?$'"));
        assert_eq!(params.values, vec![Value::from("amount")]);
        assert_eq!(
            revenue_expression(&goal(AnalyticsGoalKind::Event), &mut Params::default()),
            "NULL::numeric"
        );
    }

    #[test]
    fn stats_scan_events_once_for_all_goals() {
        let start = Utc::now() - chrono::Duration::days(7);
        let end = Utc::now();
        let mut page = goal(AnalyticsGoalKind::Page);
        page.id = 8;
        let (sql, values) = stats_query(
            &[goal(AnalyticsGoalKind::Event), page],
            1,
            start,
            end,
            Some(4),
        )
        .expect("query");
        assert_eq!(sql.matches("FROM events e").count(), 1);
        assert!(sql.contains("SELECT 7::int AS goal_id, NULL::numeric AS revenue WHERE COALESCE(e.event_name, e.event_type) = $5"));
        assert!(sql.contains(" UNION ALL SELECT 8::int AS goal_id"));
        assert!(sql.contains("e.environment_id = $4"));
        // Server-side events convert; detected bots never do.
        assert!(sql.contains(CONVERSION_TRAFFIC), "{sql}");
        let (visitors, _) = visitors_query(1, start, end, None);
        assert!(visitors.contains(VISITOR_TRAFFIC), "{visitors}");
        assert!(!visitors.contains("'unknown'"), "{visitors}");
        // project, start, end, environment, event name, page path
        assert_eq!(values.len(), 6);
        assert!(stats_query(&[], 1, start, end, None).is_none());
    }

    #[test]
    fn breakdown_attributes_sources_to_the_first_touch() {
        let start = Utc::now() - chrono::Duration::days(7);
        let end = Utc::now();
        let (sql, values) = breakdown_query(
            &goal(AnalyticsGoalKind::Event),
            AnalyticsGoalBreakdownDimension::Channel,
            1,
            start,
            end,
            None,
            10,
        )
        .expect("query");
        assert!(sql.contains("CAST(v.first_channel AS TEXT) AS value"));
        assert!(sql.contains("LEFT JOIN visitor v ON v.id = e.visitor_id"));
        assert!(sql.contains("LIMIT $5"));
        assert_eq!(values.len(), 5);

        let (sql, _) = breakdown_query(
            &goal(AnalyticsGoalKind::Event),
            AnalyticsGoalBreakdownDimension::Page,
            1,
            start,
            end,
            None,
            10,
        )
        .expect("query");
        // The page is a column of the event: no visitor or geolocation join.
        assert!(!sql.contains("JOIN visitor"));
        assert!(!sql.contains("JOIN ip_geolocations"));
    }

    #[test]
    fn percentages_stay_within_bounds() {
        assert_eq!(percentage(1, 4), 25.0);
        assert_eq!(percentage(3, 0), 0.0);
        assert_eq!(percentage(5, 4), 100.0);
    }
}
