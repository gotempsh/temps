//! Weekly digest service for aggregating data and sending weekly emails

use super::digest_data::*;
use crate::services::NotificationService;
use crate::types::{Notification, NotificationPriority, NotificationType};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use std::sync::Arc;
use temps_entities::{
    deployments, error_events, error_groups, events, funnel_steps, funnels, projects,
};
use tracing::{error, info};

pub struct DigestService {
    db: Arc<DatabaseConnection>,
    notification_service: Arc<NotificationService>,
}

impl DigestService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        notification_service: Arc<NotificationService>,
    ) -> Self {
        Self {
            db,
            notification_service,
        }
    }

    /// Generate and send weekly digest for the previous week
    pub async fn generate_and_send_weekly_digest(&self, sections: DigestSections) -> Result<()> {
        let now = Utc::now();
        let week_end = now;
        let week_start = now - Duration::days(7);

        info!(
            "Generating weekly digest for {} to {}",
            week_start.format("%Y-%m-%d"),
            week_end.format("%Y-%m-%d")
        );

        let digest_data = self
            .aggregate_digest_data(week_start, week_end, sections)
            .await?;

        // Only send if there's meaningful data
        if !digest_data.has_data() {
            info!("No data available for weekly digest, skipping send");
            return Ok(());
        }

        self.send_digest_email(digest_data).await?;

        Ok(())
    }

    /// Aggregate all digest data from various services
    async fn aggregate_digest_data(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
        sections: DigestSections,
    ) -> Result<WeeklyDigestData> {
        let mut digest = WeeklyDigestData::new(week_start, week_end);

        // Get project name (if available)
        digest.project_name = self.get_project_name().await.ok();

        // Aggregate data for each enabled section
        if sections.performance {
            digest.performance = self
                .aggregate_performance_data(week_start, week_end)
                .await
                .ok();
        }

        if sections.deployments {
            digest.deployments = self
                .aggregate_deployment_data(week_start, week_end)
                .await
                .ok();
        }

        if sections.errors {
            digest.errors = self.aggregate_error_data(week_start, week_end).await.ok();
        }

        if sections.funnels {
            digest.funnels = self.aggregate_funnel_data(week_start, week_end).await.ok();
        }

        if sections.projects {
            digest.projects = self
                .aggregate_project_data(week_start, week_end)
                .await
                .unwrap_or_default();
        }

        // Build executive summary
        digest.executive_summary = self.build_executive_summary(&digest).await?;

        Ok(digest)
    }

    /// Get project name (first project if available)
    async fn get_project_name(&self) -> Result<String> {
        let project = projects::Entity::find()
            .order_by_asc(projects::Column::Id)
            .one(self.db.as_ref())
            .await?;

        Ok(project
            .map(|p| p.name)
            .unwrap_or_else(|| "Temps".to_string()))
    }

    /// Aggregate performance and analytics data
    async fn aggregate_performance_data(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<PerformanceData> {
        // Count unique sessions this week (distinct session_id in events)
        let total_visitors = events::Entity::find()
            .filter(events::Column::Timestamp.between(week_start, week_end))
            .filter(events::Column::SessionId.is_not_null())
            .select_only()
            .column(events::Column::SessionId)
            .distinct()
            .count(self.db.as_ref())
            .await? as i64;

        // Count page views (events)
        let page_views = events::Entity::find()
            .filter(events::Column::Timestamp.between(week_start, week_end))
            .count(self.db.as_ref())
            .await? as i64;

        // Calculate previous week for comparison
        let prev_week_start = week_start - Duration::days(7);
        let prev_week_end = week_start;

        let prev_visitors = events::Entity::find()
            .filter(events::Column::Timestamp.between(prev_week_start, prev_week_end))
            .filter(events::Column::SessionId.is_not_null())
            .select_only()
            .column(events::Column::SessionId)
            .distinct()
            .count(self.db.as_ref())
            .await? as i64;

        let week_over_week_change = if prev_visitors > 0 {
            ((total_visitors - prev_visitors) as f64 / prev_visitors as f64) * 100.0
        } else {
            0.0
        };

        let average_session_duration = self
            .query_average_session_duration(week_start, week_end)
            .await
            .unwrap_or(0.0);
        let bounce_rate = self
            .query_bounce_rate(week_start, week_end)
            .await
            .unwrap_or(0.0);
        let top_pages = self
            .query_top_pages(week_start, week_end)
            .await
            .unwrap_or_default();
        let geographic_distribution = self
            .query_geographic_distribution(week_start, week_end, total_visitors)
            .await
            .unwrap_or_default();
        let visitor_trend = self
            .query_visitor_trend(week_start, week_end)
            .await
            .unwrap_or_default();

        Ok(PerformanceData {
            total_visitors,
            unique_sessions: total_visitors,
            page_views,
            average_session_duration,
            bounce_rate,
            top_pages,
            geographic_distribution,
            visitor_trend,
            week_over_week_change,
        })
    }

    /// Average session duration in minutes. A session's duration is the span
    /// from its first to its last event; sessions are then averaged.
    async fn query_average_session_duration(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<f64> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT COALESCE(AVG(session_seconds), 0)::float8 AS avg_seconds
            FROM (
                SELECT EXTRACT(EPOCH FROM (MAX(timestamp) - MIN(timestamp))) AS session_seconds
                FROM events
                WHERE timestamp BETWEEN $1 AND $2
                  AND session_id IS NOT NULL
                  AND is_crawler = false
                GROUP BY session_id
            ) s
            "#,
            [week_start.into(), week_end.into()],
        );

        let avg_seconds: f64 = self
            .db
            .query_one(stmt)
            .await?
            .and_then(|row| row.try_get::<f64>("", "avg_seconds").ok())
            .unwrap_or(0.0);

        Ok(avg_seconds / 60.0)
    }

    /// Bounce rate: percentage of sessions whose entry event is flagged as a
    /// bounce (single-interaction session).
    async fn query_bounce_rate(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<f64> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                COUNT(*) FILTER (WHERE bounced)::float8 AS bounced,
                COUNT(*)::float8 AS total
            FROM (
                SELECT bool_or(is_bounce) AS bounced
                FROM events
                WHERE timestamp BETWEEN $1 AND $2
                  AND session_id IS NOT NULL
                  AND is_crawler = false
                GROUP BY session_id
            ) s
            "#,
            [week_start.into(), week_end.into()],
        );

        if let Some(row) = self.db.query_one(stmt).await? {
            let bounced: f64 = row.try_get("", "bounced").unwrap_or(0.0);
            let total: f64 = row.try_get("", "total").unwrap_or(0.0);
            if total > 0.0 {
                return Ok((bounced / total) * 100.0);
            }
        }
        Ok(0.0)
    }

    /// Top pages by view count for the week (max 5).
    async fn query_top_pages(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<Vec<TopPage>> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                page_path,
                COUNT(*)::bigint AS views,
                COUNT(DISTINCT session_id)::bigint AS unique_visitors
            FROM events
            WHERE timestamp BETWEEN $1 AND $2
              AND is_crawler = false
            GROUP BY page_path
            ORDER BY views DESC
            LIMIT 5
            "#,
            [week_start.into(), week_end.into()],
        );

        let rows = self.db.query_all(stmt).await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(TopPage {
                    path: row.try_get("", "page_path").ok()?,
                    views: row.try_get("", "views").ok()?,
                    unique_visitors: row.try_get("", "unique_visitors").ok()?,
                })
            })
            .collect())
    }

    /// Top countries by visitor count for the week (max 5), with each
    /// country's share of `total_visitors`.
    async fn query_geographic_distribution(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
        total_visitors: i64,
    ) -> Result<Vec<GeographicData>> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                g.country AS country,
                COUNT(DISTINCT e.session_id)::bigint AS visitors
            FROM events e
            JOIN ip_geolocations g ON g.id = e.ip_geolocation_id
            WHERE e.timestamp BETWEEN $1 AND $2
              AND e.session_id IS NOT NULL
              AND e.is_crawler = false
            GROUP BY g.country
            ORDER BY visitors DESC
            LIMIT 5
            "#,
            [week_start.into(), week_end.into()],
        );

        let rows = self.db.query_all(stmt).await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let visitors: i64 = row.try_get("", "visitors").ok()?;
                let percentage = if total_visitors > 0 {
                    (visitors as f64 / total_visitors as f64) * 100.0
                } else {
                    0.0
                };
                Some(GeographicData {
                    country: row.try_get("", "country").ok()?,
                    visitors,
                    percentage,
                })
            })
            .collect())
    }

    /// Daily unique-session counts across the digest window, for the trend
    /// sparkline.
    async fn query_visitor_trend(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<Vec<TrendPoint>> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                date_trunc('day', timestamp) AS day,
                COUNT(DISTINCT session_id)::bigint AS visitors
            FROM events
            WHERE timestamp BETWEEN $1 AND $2
              AND session_id IS NOT NULL
              AND is_crawler = false
            GROUP BY day
            ORDER BY day ASC
            "#,
            [week_start.into(), week_end.into()],
        );

        let rows = self.db.query_all(stmt).await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(TrendPoint {
                    date: row.try_get("", "day").ok()?,
                    value: row.try_get("", "visitors").ok()?,
                })
            })
            .collect())
    }

    /// Aggregate deployment and infrastructure data
    async fn aggregate_deployment_data(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<DeploymentData> {
        // Count total deployments
        let total_deployments = deployments::Entity::find()
            .filter(deployments::Column::CreatedAt.between(week_start, week_end))
            .count(self.db.as_ref())
            .await? as i64;

        // Count successful vs failed deployments
        let successful_deployments = deployments::Entity::find()
            .filter(deployments::Column::CreatedAt.between(week_start, week_end))
            .filter(deployments::Column::State.eq("completed"))
            .count(self.db.as_ref())
            .await? as i64;

        let failed_deployments = deployments::Entity::find()
            .filter(deployments::Column::CreatedAt.between(week_start, week_end))
            .filter(deployments::Column::State.eq("failed"))
            .count(self.db.as_ref())
            .await? as i64;

        let success_rate = if total_deployments > 0 {
            (successful_deployments as f64 / total_deployments as f64) * 100.0
        } else {
            0.0
        };

        Ok(DeploymentData {
            total_deployments,
            successful_deployments,
            failed_deployments,
            success_rate,
            average_duration: 0.0,
            preview_environments_created: 0,
            preview_environments_destroyed: 0,
            most_active_projects: vec![],
            deployment_trend: vec![],
        })
    }

    /// Aggregate error and reliability data from `error_events`,
    /// `error_groups`, and `external_service_health_checks`.
    async fn aggregate_error_data(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<ErrorData> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        // Total error events captured in the window.
        let total_errors = error_events::Entity::find()
            .filter(error_events::Column::Timestamp.between(week_start, week_end))
            .count(self.db.as_ref())
            .await? as i64;

        // Error groups first seen this week — "new error types".
        let new_error_types = error_groups::Entity::find()
            .filter(error_groups::Column::FirstSeen.between(week_start, week_end))
            .count(self.db.as_ref())
            .await? as i64;

        // Distinct visitors affected by errors this week.
        let affected_users = error_events::Entity::find()
            .filter(error_events::Column::Timestamp.between(week_start, week_end))
            .filter(error_events::Column::VisitorId.is_not_null())
            .select_only()
            .column(error_events::Column::VisitorId)
            .distinct()
            .count(self.db.as_ref())
            .await? as i64;

        // Most common errors this week, grouped by error group.
        let most_common_errors = self
            .query_most_common_errors(week_start, week_end)
            .await
            .unwrap_or_default();

        // Daily error counts for the trend sparkline.
        let error_trend = self
            .query_error_trend(week_start, week_end)
            .await
            .unwrap_or_default();

        // Health-check based uptime. `external_service_health_checks.status`
        // is "operational" | "degraded" | "down". Uptime = share of checks
        // that were operational; failed = degraded + down.
        let (uptime_percentage, failed_health_checks) = {
            let stmt = Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                SELECT
                    COUNT(*) FILTER (WHERE status = 'operational')::float8 AS operational,
                    COUNT(*) FILTER (WHERE status <> 'operational')::bigint AS failed,
                    COUNT(*)::float8 AS total
                FROM external_service_health_checks
                WHERE checked_at BETWEEN $1 AND $2
                "#,
                [week_start.into(), week_end.into()],
            );
            match self.db.query_one(stmt).await? {
                Some(row) => {
                    let operational: f64 = row.try_get("", "operational").unwrap_or(0.0);
                    let failed: i64 = row.try_get("", "failed").unwrap_or(0);
                    let total: f64 = row.try_get("", "total").unwrap_or(0.0);
                    if total > 0.0 {
                        ((operational / total) * 100.0, failed)
                    } else {
                        // No health checks recorded — report 100% rather than
                        // a fabricated 99.9, and zero failures.
                        (100.0, 0)
                    }
                }
                None => (100.0, 0),
            }
        };

        // Error rate: errors per 1,000 page views this week. Gives the number
        // meaning relative to traffic instead of a raw count.
        let page_views = events::Entity::find()
            .filter(events::Column::Timestamp.between(week_start, week_end))
            .count(self.db.as_ref())
            .await? as i64;
        let error_rate = if page_views > 0 {
            (total_errors as f64 / page_views as f64) * 1000.0
        } else {
            0.0
        };

        Ok(ErrorData {
            total_errors,
            error_rate,
            new_error_types,
            most_common_errors,
            affected_users,
            error_trend,
            uptime_percentage,
            failed_health_checks,
        })
    }

    /// Top error groups by event count this week (max 5).
    async fn query_most_common_errors(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<Vec<CommonError>> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                g.title AS error_type,
                COUNT(e.id)::bigint AS count,
                MIN(e.timestamp) AS first_occurrence,
                MAX(e.timestamp) AS last_occurrence,
                COUNT(DISTINCT e.visitor_id)::bigint AS affected_sessions
            FROM error_events e
            JOIN error_groups g ON g.id = e.error_group_id
            WHERE e.timestamp BETWEEN $1 AND $2
            GROUP BY g.id, g.title
            ORDER BY count DESC
            LIMIT 5
            "#,
            [week_start.into(), week_end.into()],
        );

        let rows = self.db.query_all(stmt).await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(CommonError {
                    error_type: row.try_get("", "error_type").ok()?,
                    count: row.try_get("", "count").ok()?,
                    first_occurrence: row.try_get("", "first_occurrence").ok()?,
                    last_occurrence: row.try_get("", "last_occurrence").ok()?,
                    affected_sessions: row.try_get("", "affected_sessions").ok()?,
                })
            })
            .collect())
    }

    /// Daily error-event counts across the digest window.
    async fn query_error_trend(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<Vec<TrendPoint>> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT
                date_trunc('day', timestamp) AS day,
                COUNT(*)::bigint AS errors
            FROM error_events
            WHERE timestamp BETWEEN $1 AND $2
            GROUP BY day
            ORDER BY day ASC
            "#,
            [week_start.into(), week_end.into()],
        );

        let rows = self.db.query_all(stmt).await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(TrendPoint {
                    date: row.try_get("", "day").ok()?,
                    value: row.try_get("", "errors").ok()?,
                })
            })
            .collect())
    }

    /// Aggregate funnel and conversion data from `funnels` / `funnel_steps`.
    ///
    /// For each active funnel, a session "enters" if it fired the first step's
    /// event and "completes" if it also fired the last step's event within the
    /// window. Conversion is completions / entries, compared against the prior
    /// week for the trend.
    async fn aggregate_funnel_data(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<FunnelData> {
        let funnels = funnels::Entity::find()
            .filter(funnels::Column::IsActive.eq(true))
            .all(self.db.as_ref())
            .await?;

        let total_funnels = funnels.len() as i64;
        let prev_week_start = week_start - Duration::days(7);

        let mut funnel_stats = Vec::new();
        for funnel in funnels {
            let steps = funnel_steps::Entity::find()
                .filter(funnel_steps::Column::FunnelId.eq(funnel.id))
                .order_by_asc(funnel_steps::Column::StepOrder)
                .all(self.db.as_ref())
                .await?;

            // A funnel needs at least one step to be measurable.
            let Some(first_step) = steps.first() else {
                continue;
            };
            let last_step = steps.last().unwrap_or(first_step);

            let (entries, completions) = self
                .funnel_entries_completions(
                    funnel.id,
                    &first_step.event_name,
                    &last_step.event_name,
                    week_start,
                    week_end,
                )
                .await
                .unwrap_or((0, 0));

            let (prev_entries, prev_completions) = self
                .funnel_entries_completions(
                    funnel.id,
                    &first_step.event_name,
                    &last_step.event_name,
                    prev_week_start,
                    week_start,
                )
                .await
                .unwrap_or((0, 0));

            let completion_rate = if entries > 0 {
                (completions as f64 / entries as f64) * 100.0
            } else {
                0.0
            };
            let prev_rate = if prev_entries > 0 {
                (prev_completions as f64 / prev_entries as f64) * 100.0
            } else {
                0.0
            };
            let week_over_week_change = completion_rate - prev_rate;

            funnel_stats.push(FunnelStat {
                funnel_name: funnel.name,
                completion_rate,
                drop_off_rate: 100.0 - completion_rate,
                week_over_week_change,
                total_entries: entries,
                total_completions: completions,
            });
        }

        // Most-trafficked funnels first.
        funnel_stats.sort_by_key(|f| std::cmp::Reverse(f.total_entries));

        Ok(FunnelData {
            total_funnels,
            funnel_stats,
        })
    }

    /// Count sessions that entered (fired `first_event`) and completed (also
    /// fired `last_event`) a funnel within `[start, end)`.
    async fn funnel_entries_completions(
        &self,
        funnel_id: i32,
        first_event: &str,
        last_event: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(i64, i64)> {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

        // Single funnel-step funnels: entry == completion.
        let same_step = first_event == last_event;

        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            WITH entered AS (
                SELECT DISTINCT session_id
                FROM events
                WHERE timestamp >= $1 AND timestamp < $2
                  AND session_id IS NOT NULL
                  AND is_crawler = false
                  AND COALESCE(event_name, event_type) = $3
            ),
            completed AS (
                SELECT DISTINCT session_id
                FROM events
                WHERE timestamp >= $1 AND timestamp < $2
                  AND session_id IS NOT NULL
                  AND is_crawler = false
                  AND COALESCE(event_name, event_type) = $4
            )
            SELECT
                (SELECT COUNT(*) FROM entered)::bigint AS entries,
                (SELECT COUNT(*) FROM entered e
                    WHERE $5 OR e.session_id IN (SELECT session_id FROM completed))::bigint
                    AS completions
            "#,
            [
                start.into(),
                end.into(),
                first_event.into(),
                last_event.into(),
                same_step.into(),
            ],
        );

        // `funnel_id` is accepted for future per-funnel event filtering; the
        // current model identifies funnel membership purely by event name.
        let _ = funnel_id;

        if let Some(row) = self.db.query_one(stmt).await? {
            let entries: i64 = row.try_get("", "entries").unwrap_or(0);
            let completions: i64 = row.try_get("", "completions").unwrap_or(0);
            return Ok((entries, completions));
        }
        Ok((0, 0))
    }

    /// Aggregate individual project statistics
    async fn aggregate_project_data(
        &self,
        week_start: DateTime<Utc>,
        week_end: DateTime<Utc>,
    ) -> Result<Vec<ProjectStats>> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
        use temps_entities::{deployments, events, projects};

        // Get all projects
        let all_projects = projects::Entity::find().all(self.db.as_ref()).await?;

        let mut project_stats = Vec::new();

        for project in all_projects {
            // Count unique sessions for this project
            let visitors = events::Entity::find()
                .filter(events::Column::ProjectId.eq(project.id))
                .filter(events::Column::Timestamp.between(week_start, week_end))
                .filter(events::Column::SessionId.is_not_null())
                .select_only()
                .column(events::Column::SessionId)
                .distinct()
                .count(self.db.as_ref())
                .await? as i64;

            // Count page views for this project
            let page_views = events::Entity::find()
                .filter(events::Column::ProjectId.eq(project.id))
                .filter(events::Column::Timestamp.between(week_start, week_end))
                .count(self.db.as_ref())
                .await? as i64;

            // Count deployments for this project
            let deployment_count = deployments::Entity::find()
                .filter(deployments::Column::ProjectId.eq(project.id))
                .filter(deployments::Column::CreatedAt.between(week_start, week_end))
                .count(self.db.as_ref())
                .await? as i64;

            // Calculate previous week visitors for trend
            let prev_week_start = week_start - Duration::days(7);
            let prev_week_end = week_start;

            let prev_visitors = events::Entity::find()
                .filter(events::Column::ProjectId.eq(project.id))
                .filter(events::Column::Timestamp.between(prev_week_start, prev_week_end))
                .filter(events::Column::SessionId.is_not_null())
                .select_only()
                .column(events::Column::SessionId)
                .distinct()
                .count(self.db.as_ref())
                .await? as i64;

            let week_over_week_change = if prev_visitors > 0 {
                ((visitors - prev_visitors) as f64 / prev_visitors as f64) * 100.0
            } else if visitors > 0 {
                100.0 // If we had 0 before and now have some, that's 100% increase
            } else {
                0.0
            };

            // Only include projects that have activity
            if visitors > 0 || page_views > 0 || deployment_count > 0 {
                project_stats.push(ProjectStats {
                    project_id: project.id,
                    project_name: project.name.clone(),
                    project_slug: project.slug.clone(),
                    visitors,
                    page_views,
                    unique_sessions: visitors, // Same as visitors (unique sessions)
                    deployments: deployment_count,
                    week_over_week_change,
                });
            }
        }

        // Sort projects by visitors (most active first)
        project_stats.sort_by_key(|p| std::cmp::Reverse(p.visitors));

        Ok(project_stats)
    }

    /// Build executive summary from aggregated data
    async fn build_executive_summary(&self, digest: &WeeklyDigestData) -> Result<ExecutiveSummary> {
        let total_visitors = digest
            .performance
            .as_ref()
            .map(|p| p.total_visitors)
            .unwrap_or(0);

        let visitor_change_percent = digest
            .performance
            .as_ref()
            .map(|p| p.week_over_week_change)
            .unwrap_or(0.0);

        let total_deployments = digest
            .deployments
            .as_ref()
            .map(|d| d.total_deployments)
            .unwrap_or(0);

        let failed_deployments = digest
            .deployments
            .as_ref()
            .map(|d| d.failed_deployments)
            .unwrap_or(0);

        let new_errors = digest
            .errors
            .as_ref()
            .map(|e| e.new_error_types)
            .unwrap_or(0);

        let uptime_percent = digest
            .errors
            .as_ref()
            .map(|e| e.uptime_percentage)
            .unwrap_or(100.0);

        Ok(ExecutiveSummary {
            total_visitors,
            visitor_change_percent,
            total_deployments,
            failed_deployments,
            new_errors,
            uptime_percent,
        })
    }

    /// Send digest email using notification service
    async fn send_digest_email(&self, digest: WeeklyDigestData) -> Result<()> {
        let subject = format!(
            "📊 Weekly Digest - {} to {}",
            digest.week_start.format("%b %d"),
            digest.week_end.format("%b %d, %Y")
        );

        let html_body = super::templates::render_html_template(&digest)?;
        let text_body = super::templates::render_text_template(&digest)?;

        // Create notification with HTML body (email provider will handle it)
        let notification = Notification {
            id: uuid::Uuid::new_v4().to_string(),
            title: subject,
            message: html_body,
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: [("text_body".to_string(), text_body)].into_iter().collect(),
            bypass_throttling: true, // Weekly digest should always send
        };

        self.notification_service
            .send_notification(notification)
            .await
            .map_err(|e| {
                error!("Failed to send weekly digest email: {}", e);
                anyhow::anyhow!("Failed to send weekly digest: {}", e)
            })?;

        info!("Weekly digest email sent successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_core::EncryptionService;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{deployments, environments, events, projects, users};

    async fn setup_test_service() -> Result<(DigestService, TestDatabase), String> {
        let test_db = TestDatabase::with_migrations()
            .await
            .map_err(|error| format!("Failed to create test database: {error}"))?;

        let encryption_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encryption_service = Arc::new(
            EncryptionService::new(encryption_key).expect("Failed to create encryption service"),
        );

        let notification_service = Arc::new(NotificationService::new(
            test_db.connection_arc(),
            encryption_service,
        ));

        let digest_service = DigestService::new(test_db.connection_arc(), notification_service);

        Ok((digest_service, test_db))
    }

    macro_rules! setup_test_service_or_skip {
        () => {
            match setup_test_service().await {
                Ok(setup) => setup,
                Err(error) => {
                    if temps_database::test_utils::is_container_runtime_unavailable(&error) {
                        eprintln!("Skipping Docker-dependent digest test: {error}");
                        return;
                    }
                    panic!("Failed to set up digest test database: {error}");
                }
            }
        };
    }

    #[tokio::test]
    async fn test_generate_weekly_digest_empty_data() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);
        let sections = DigestSections::default();

        let digest = service
            .aggregate_digest_data(week_start, now, sections)
            .await
            .expect("Failed to aggregate digest data");

        // Should have basic structure even with no data
        assert_eq!(digest.week_start, week_start);
        assert_eq!(digest.executive_summary.total_visitors, 0);
        assert_eq!(digest.executive_summary.total_deployments, 0);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_performance_data_empty() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let perf = service
            .aggregate_performance_data(week_start, now)
            .await
            .expect("Failed to aggregate performance data");

        assert_eq!(perf.total_visitors, 0);
        assert_eq!(perf.page_views, 0);
        assert_eq!(perf.week_over_week_change, 0.0);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_deployment_data_empty() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let deploy = service
            .aggregate_deployment_data(week_start, now)
            .await
            .expect("Failed to aggregate deployment data");

        assert_eq!(deploy.total_deployments, 0);
        assert_eq!(deploy.successful_deployments, 0);
        assert_eq!(deploy.failed_deployments, 0);
        assert_eq!(deploy.success_rate, 0.0);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_project_data_empty() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let projects = service
            .aggregate_project_data(week_start, now)
            .await
            .expect("Failed to aggregate project data");

        assert_eq!(projects.len(), 0);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_digest_has_data() {
        let now = Utc::now();
        let week_start = now - Duration::days(7);

        // Empty digest
        let empty_digest = WeeklyDigestData::new(week_start, now);
        assert!(!empty_digest.has_data());

        // Digest with performance data
        let mut digest_with_data = WeeklyDigestData::new(week_start, now);
        digest_with_data.performance = Some(PerformanceData {
            total_visitors: 100,
            unique_sessions: 100,
            page_views: 500,
            average_session_duration: 5.0,
            bounce_rate: 30.0,
            top_pages: vec![],
            geographic_distribution: vec![],
            visitor_trend: vec![],
            week_over_week_change: 10.0,
        });
        assert!(digest_with_data.has_data());
    }

    #[tokio::test]
    async fn test_executive_summary_calculation() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let mut digest = WeeklyDigestData::new(week_start, now);
        digest.performance = Some(PerformanceData {
            total_visitors: 1234,
            unique_sessions: 1234,
            page_views: 5678,
            average_session_duration: 5.5,
            bounce_rate: 25.0,
            top_pages: vec![],
            geographic_distribution: vec![],
            visitor_trend: vec![],
            week_over_week_change: 15.0,
        });

        digest.deployments = Some(DeploymentData {
            total_deployments: 45,
            successful_deployments: 42,
            failed_deployments: 3,
            success_rate: 93.3,
            average_duration: 2.5,
            preview_environments_created: 10,
            preview_environments_destroyed: 8,
            most_active_projects: vec![],
            deployment_trend: vec![],
        });

        let summary = service
            .build_executive_summary(&digest)
            .await
            .expect("Failed to build executive summary");

        assert_eq!(summary.total_visitors, 1234);
        assert_eq!(summary.visitor_change_percent, 15.0);
        assert_eq!(summary.total_deployments, 45);
        assert_eq!(summary.failed_deployments, 3);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    // Integration tests with real data
    #[tokio::test]
    async fn test_aggregate_performance_with_real_sessions() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        // Create test project first
        let project = projects::ActiveModel {
            name: Set("test-project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        // Create test environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        // Create a deployment first for events to reference
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deployment-test".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let deployment = deployment.insert(test_db.connection()).await.unwrap();

        // Create test events with session_id in current week (5 unique sessions)
        for i in 0..5 {
            let event = events::ActiveModel {
                timestamp: Set(now - Duration::hours(i as i64)),
                project_id: Set(project.id),
                environment_id: Set(Some(environment.id)),
                deployment_id: Set(Some(deployment.id)),
                session_id: Set(Some(format!("session_{}", i))),
                hostname: Set("example.com".to_string()),
                pathname: Set("/".to_string()),
                page_path: Set("/".to_string()),
                href: Set("https://example.com/".to_string()),
                is_entry: Set(true),
                is_exit: Set(false),
                is_bounce: Set(false),
                event_type: Set("pageview".to_string()),
                is_crawler: Set(false),
                ..Default::default()
            };
            event.insert(test_db.connection()).await.unwrap();
        }

        // Create test events in previous week (3 unique sessions)
        for i in 0..3 {
            let event = events::ActiveModel {
                timestamp: Set(week_start - Duration::hours((i + 1) as i64)),
                project_id: Set(project.id),
                environment_id: Set(Some(environment.id)),
                deployment_id: Set(Some(deployment.id)),
                session_id: Set(Some(format!("prev_session_{}", i))),
                hostname: Set("example.com".to_string()),
                pathname: Set("/".to_string()),
                page_path: Set("/".to_string()),
                href: Set("https://example.com/".to_string()),
                is_entry: Set(true),
                is_exit: Set(false),
                is_bounce: Set(false),
                event_type: Set("pageview".to_string()),
                is_crawler: Set(false),
                ..Default::default()
            };
            event.insert(test_db.connection()).await.unwrap();
        }

        let perf = service
            .aggregate_performance_data(week_start, now)
            .await
            .expect("Failed to aggregate performance data");

        assert_eq!(perf.total_visitors, 5);
        assert_eq!(perf.unique_sessions, 5);
        assert_eq!(perf.page_views, 5); // 5 events this week

        // Week over week change: (5 - 3) / 3 * 100 = 66.67%
        assert!((perf.week_over_week_change - 66.67).abs() < 0.1);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_deployment_with_real_data() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        // Create test project first
        let project = projects::ActiveModel {
            name: Set("test-project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        // Create test environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        // Create successful deployments
        for i in 0..7 {
            let deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(format!("deployment-{}", i)),
                state: Set("completed".to_string()),
                metadata: Set(Some(Default::default())),
                created_at: Set(now - Duration::hours(i as i64)),
                updated_at: Set(now - Duration::hours(i as i64)),
                ..Default::default()
            };
            deployment.insert(test_db.connection()).await.unwrap();
        }

        // Create failed deployments
        for i in 0..2 {
            let deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(format!("deployment-failed-{}", i)),
                state: Set("failed".to_string()),
                metadata: Set(Some(Default::default())),
                created_at: Set(now - Duration::hours((i + 10) as i64)),
                updated_at: Set(now - Duration::hours((i + 10) as i64)),
                ..Default::default()
            };
            deployment.insert(test_db.connection()).await.unwrap();
        }

        let deploy_data = service
            .aggregate_deployment_data(week_start, now)
            .await
            .expect("Failed to aggregate deployment data");

        assert_eq!(deploy_data.total_deployments, 9);
        assert_eq!(deploy_data.successful_deployments, 7);
        assert_eq!(deploy_data.failed_deployments, 2);
        assert!((deploy_data.success_rate - 77.78).abs() < 0.1); // 7/9 * 100 = 77.78%

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_project_data_with_activity() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        // Create test project
        let project = projects::ActiveModel {
            name: Set("test-project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        // Create test environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        // Create test deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deploy-1".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            commit_sha: Set(Some("abc123".to_string())),
            branch_ref: Set(Some("refs/heads/main".to_string())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let deployment = deployment.insert(test_db.connection()).await.unwrap();

        // Create test events (simulating visitors and page views)
        for i in 0..5 {
            let event = events::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(Some(environment.id)),
                deployment_id: Set(Some(deployment.id)),
                session_id: Set(Some(format!("session-{}", i))),
                event_type: Set("pageview".to_string()),
                timestamp: Set(now - Duration::hours(i as i64)),
                hostname: Set("example.com".to_string()),
                pathname: Set("/".to_string()),
                page_path: Set("/".to_string()),
                href: Set("https://example.com/".to_string()),
                ..Default::default()
            };
            event.insert(test_db.connection()).await.unwrap();
        }

        let projects_data = service
            .aggregate_project_data(week_start, now)
            .await
            .expect("Failed to aggregate project data");

        assert_eq!(projects_data.len(), 1);
        assert_eq!(projects_data[0].project_name, "test-project");
        assert!(projects_data[0].visitors > 0);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_full_digest_integration() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        // Create test data across multiple entities

        // Create project
        let project = projects::ActiveModel {
            name: Set("integration-test-project".to_string()),
            slug: Set("integration-test-project".to_string()),
            repo_name: Set("integration-test-repo".to_string()),
            repo_owner: Set("integration-test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        // Create test environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        // Create a deployment first for events to reference
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deployment-initial".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let deployment = deployment.insert(test_db.connection()).await.unwrap();

        // Create events for session tracking
        for i in 0..10 {
            let event = events::ActiveModel {
                timestamp: Set(now - Duration::hours(i as i64)),
                project_id: Set(project.id),
                environment_id: Set(Some(environment.id)),
                deployment_id: Set(Some(deployment.id)),
                session_id: Set(Some(format!("int_session_{}", i))),
                hostname: Set("example.com".to_string()),
                pathname: Set("/".to_string()),
                page_path: Set("/".to_string()),
                href: Set("https://example.com/".to_string()),
                is_entry: Set(true),
                is_exit: Set(false),
                is_bounce: Set(false),
                event_type: Set("pageview".to_string()),
                is_crawler: Set(false),
                ..Default::default()
            };
            event.insert(test_db.connection()).await.unwrap();
        }

        // Create additional deployments
        for i in 0..5 {
            let deployment = deployments::ActiveModel {
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                slug: Set(format!("deployment-additional-{}", i)),
                state: Set(if i < 4 { "completed" } else { "failed" }.to_string()),
                metadata: Set(Some(Default::default())),
                created_at: Set(now - Duration::hours(i as i64)),
                updated_at: Set(now - Duration::hours(i as i64)),
                ..Default::default()
            };
            deployment.insert(test_db.connection()).await.unwrap();
        }

        // Create users
        for i in 0..2 {
            let user = users::ActiveModel {
                name: Set(format!("Integration User {}", i)),
                email: Set(format!("int_user{}@example.com", i)),
                password_hash: Set(Some("hash".to_string())),
                created_at: Set(now - Duration::hours(i as i64)),
                updated_at: Set(now),
                ..Default::default()
            };
            user.insert(test_db.connection()).await.unwrap();
        }

        // Generate full digest
        let sections = DigestSections::default();
        let digest = service
            .aggregate_digest_data(week_start, now, sections)
            .await
            .expect("Failed to generate full digest");

        // Verify all sections have data
        assert!(digest.has_data());
        assert!(digest.performance.is_some());
        assert!(digest.deployments.is_some());
        assert!(!digest.projects.is_empty());

        // Verify performance data
        let perf = digest.performance.unwrap();
        assert_eq!(perf.total_visitors, 10);

        // Verify deployment data
        let deploy = digest.deployments.unwrap();
        assert_eq!(deploy.total_deployments, 6); // 1 initial + 5 additional
        assert_eq!(deploy.successful_deployments, 5); // 1 initial + 4 from loop
        assert_eq!(deploy.failed_deployments, 1);

        // Verify project data
        assert_eq!(digest.projects.len(), 1);
        assert_eq!(digest.projects[0].project_name, "integration-test-project");

        // Verify executive summary
        assert_eq!(digest.executive_summary.total_visitors, 10);
        assert_eq!(digest.executive_summary.total_deployments, 6);
        assert_eq!(digest.executive_summary.failed_deployments, 1);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    // ── Error aggregation ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_aggregate_error_data_empty() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let errors = service
            .aggregate_error_data(week_start, now)
            .await
            .expect("Failed to aggregate error data");

        // With no error events and no health checks, the digest must report
        // zeros and 100% uptime — never the old fabricated 99.9%.
        assert_eq!(errors.total_errors, 0);
        assert_eq!(errors.new_error_types, 0);
        assert_eq!(errors.affected_users, 0);
        assert_eq!(errors.failed_health_checks, 0);
        assert_eq!(errors.uptime_percentage, 100.0);
        assert!(errors.most_common_errors.is_empty());

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_error_data_with_real_errors() {
        use temps_entities::{error_events, error_groups, visitor};

        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let project = projects::ActiveModel {
            name: Set("err-project".to_string()),
            slug: Set("err-project".to_string()),
            repo_name: Set("err-repo".to_string()),
            repo_owner: Set("err-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        // error_events.visitor_id has an FK to `visitor` — create two.
        let mut visitor_ids = Vec::new();
        for i in 0..2 {
            let v = visitor::ActiveModel {
                visitor_id: Set(format!("visitor-{}", i)),
                project_id: Set(project.id),
                environment_id: Set(environment.id),
                first_seen: Set(now - Duration::days(1)),
                last_seen: Set(now),
                is_crawler: Set(false),
                has_activity: Set(true),
                ..Default::default()
            };
            visitor_ids.push(v.insert(test_db.connection()).await.unwrap().id);
        }

        // An error group first seen this week → counts as a new error type.
        let group = error_groups::ActiveModel {
            title: Set("TypeError: undefined is not a function".to_string()),
            error_type: Set("TypeError".to_string()),
            first_seen: Set(now - Duration::days(2)),
            last_seen: Set(now),
            total_count: Set(3),
            status: Set("unresolved".to_string()),
            project_id: Set(project.id),
            created_at: Set(now - Duration::days(2)),
            updated_at: Set(now),
            ..Default::default()
        };
        let group = group.insert(test_db.connection()).await.unwrap();

        // Three error events this week, two distinct visitors.
        for i in 0..3 {
            let event = error_events::ActiveModel {
                error_group_id: Set(group.id),
                project_id: Set(project.id),
                fingerprint_hash: Set(format!("fp-{}", i)),
                timestamp: Set(now - Duration::hours((i + 1) as i64)),
                exception_type: Set("TypeError".to_string()),
                exception_value: Set(Some("undefined is not a function".to_string())),
                source: Set(Some("custom".to_string())),
                visitor_id: Set(Some(if i < 2 {
                    visitor_ids[0]
                } else {
                    visitor_ids[1]
                })),
                created_at: Set(now - Duration::hours((i + 1) as i64)),
                ..Default::default()
            };
            event.insert(test_db.connection()).await.unwrap();
        }

        let errors = service
            .aggregate_error_data(week_start, now)
            .await
            .expect("Failed to aggregate error data");

        assert_eq!(errors.total_errors, 3);
        assert_eq!(errors.new_error_types, 1);
        assert_eq!(errors.affected_users, 2);
        assert_eq!(errors.most_common_errors.len(), 1);
        assert_eq!(errors.most_common_errors[0].count, 3);
        // No health checks recorded → 100% uptime, not a fabricated value.
        assert_eq!(errors.uptime_percentage, 100.0);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_error_data_uptime_from_health_checks() {
        use temps_entities::{external_service_health_checks as hc, external_services};

        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        // Health checks have an FK to external_services — create one first.
        let svc = external_services::ActiveModel {
            name: Set("test-postgres".to_string()),
            service_type: Set("postgres".to_string()),
            status: Set("running".to_string()),
            topology: Set("standalone".to_string()),
            consecutive_health_failures: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let svc = svc.insert(test_db.connection()).await.unwrap();

        // 8 operational + 2 down = 80% uptime, 2 failed checks.
        for i in 0..10 {
            let status = if i < 8 { "operational" } else { "down" };
            let check = hc::ActiveModel {
                service_id: Set(svc.id),
                checked_at: Set(now - Duration::hours((i + 1) as i64)),
                status: Set(status.to_string()),
                response_time_ms: Set(Some(100)),
                error_message: Set(None),
                ..Default::default()
            };
            check.insert(test_db.connection()).await.unwrap();
        }

        let errors = service
            .aggregate_error_data(week_start, now)
            .await
            .expect("Failed to aggregate error data");

        assert!((errors.uptime_percentage - 80.0).abs() < 0.01);
        assert_eq!(errors.failed_health_checks, 2);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    // ── Funnel aggregation ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_aggregate_funnel_data_empty() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let funnels = service
            .aggregate_funnel_data(week_start, now)
            .await
            .expect("Failed to aggregate funnel data");

        assert_eq!(funnels.total_funnels, 0);
        assert!(funnels.funnel_stats.is_empty());

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    #[tokio::test]
    async fn test_aggregate_funnel_data_with_conversions() {
        use temps_entities::{funnel_steps, funnels};

        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let project = projects::ActiveModel {
            name: Set("funnel-project".to_string()),
            slug: Set("funnel-project".to_string()),
            repo_name: Set("funnel-repo".to_string()),
            repo_owner: Set("funnel-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deploy-funnel".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let deployment = deployment.insert(test_db.connection()).await.unwrap();

        // Funnel: signup_started → signup_completed.
        let funnel = funnels::ActiveModel {
            project_id: Set(project.id),
            name: Set("Signup".to_string()),
            description: Set(None),
            is_active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let funnel = funnel.insert(test_db.connection()).await.unwrap();

        for (order, event_name) in [(1, "signup_started"), (2, "signup_completed")] {
            let step = funnel_steps::ActiveModel {
                funnel_id: Set(funnel.id),
                step_order: Set(order),
                event_name: Set(event_name.to_string()),
                event_filter: Set(None),
                created_at: Set(now),
                ..Default::default()
            };
            step.insert(test_db.connection()).await.unwrap();
        }

        // Helper to insert a custom event for a session.
        let insert_event =
            |session: &str, event_name: &str, ts: DateTime<Utc>| events::ActiveModel {
                timestamp: Set(ts),
                project_id: Set(project.id),
                environment_id: Set(Some(environment.id)),
                deployment_id: Set(Some(deployment.id)),
                session_id: Set(Some(session.to_string())),
                hostname: Set("example.com".to_string()),
                pathname: Set("/".to_string()),
                page_path: Set("/".to_string()),
                href: Set("https://example.com/".to_string()),
                event_type: Set("custom".to_string()),
                event_name: Set(Some(event_name.to_string())),
                is_crawler: Set(false),
                ..Default::default()
            };

        // 4 sessions enter, 3 of them complete this week. Events are placed
        // strictly inside the window — funnel aggregation uses `< week_end`.
        for i in 0..4 {
            let sid = format!("s{}", i);
            let ts = now - Duration::hours((i + 1) as i64);
            insert_event(&sid, "signup_started", ts)
                .insert(test_db.connection())
                .await
                .unwrap();
            if i < 3 {
                insert_event(&sid, "signup_completed", ts)
                    .insert(test_db.connection())
                    .await
                    .unwrap();
            }
        }

        let funnel_data = service
            .aggregate_funnel_data(week_start, now)
            .await
            .expect("Failed to aggregate funnel data");

        assert_eq!(funnel_data.total_funnels, 1);
        assert_eq!(funnel_data.funnel_stats.len(), 1);
        let stat = &funnel_data.funnel_stats[0];
        assert_eq!(stat.funnel_name, "Signup");
        assert_eq!(stat.total_entries, 4);
        assert_eq!(stat.total_completions, 3);
        assert!((stat.completion_rate - 75.0).abs() < 0.01);
        assert!((stat.drop_off_rate - 25.0).abs() < 0.01);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }

    // ── Performance detail aggregation ──────────────────────────────────

    #[tokio::test]
    async fn test_aggregate_performance_top_pages_and_bounce() {
        let (service, test_db) = setup_test_service_or_skip!();

        let now = Utc::now();
        let week_start = now - Duration::days(7);

        let project = projects::ActiveModel {
            name: Set("perf-project".to_string()),
            slug: Set("perf-project".to_string()),
            repo_name: Set("perf-repo".to_string()),
            repo_owner: Set("perf-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Astro),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let project = project.insert(test_db.connection()).await.unwrap();

        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("production".to_string()),
            host: Set("production.example.com".to_string()),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let environment = environment.insert(test_db.connection()).await.unwrap();

        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("deploy-perf".to_string()),
            state: Set("completed".to_string()),
            metadata: Set(Some(Default::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let deployment = deployment.insert(test_db.connection()).await.unwrap();

        // 3 sessions: 2 land on /pricing (one bounces), 1 on /docs.
        let pages = [
            ("sess-a", "/pricing", true),
            ("sess-b", "/pricing", false),
            ("sess-c", "/docs", false),
        ];
        for (sid, path, bounce) in pages {
            let event = events::ActiveModel {
                timestamp: Set(now - Duration::hours(1)),
                project_id: Set(project.id),
                environment_id: Set(Some(environment.id)),
                deployment_id: Set(Some(deployment.id)),
                session_id: Set(Some(sid.to_string())),
                hostname: Set("example.com".to_string()),
                pathname: Set(path.to_string()),
                page_path: Set(path.to_string()),
                href: Set(format!("https://example.com{}", path)),
                is_entry: Set(true),
                is_bounce: Set(bounce),
                event_type: Set("pageview".to_string()),
                is_crawler: Set(false),
                ..Default::default()
            };
            event.insert(test_db.connection()).await.unwrap();
        }

        let perf = service
            .aggregate_performance_data(week_start, now)
            .await
            .expect("Failed to aggregate performance data");

        // Top pages: /pricing has 2 views, /docs has 1.
        assert_eq!(perf.top_pages.len(), 2);
        assert_eq!(perf.top_pages[0].path, "/pricing");
        assert_eq!(perf.top_pages[0].views, 2);
        // Bounce rate: 1 of 3 sessions bounced.
        assert!((perf.bounce_rate - 33.33).abs() < 0.1);

        test_db.cleanup_all_tables().await.expect("Cleanup failed");
    }
}
