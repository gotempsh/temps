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
use temps_entities::{deployments, events, projects};
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

        // TODO: Implement more detailed analytics queries
        // For now, return basic data
        Ok(PerformanceData {
            total_visitors,
            unique_sessions: total_visitors,
            page_views,
            average_session_duration: 0.0,
            bounce_rate: 0.0,
            top_pages: vec![],
            geographic_distribution: vec![],
            visitor_trend: vec![],
            week_over_week_change,
        })
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

    /// Aggregate error and reliability data
    async fn aggregate_error_data(
        &self,
        _week_start: DateTime<Utc>,
        _week_end: DateTime<Utc>,
    ) -> Result<ErrorData> {
        // TODO: Implement error aggregation from temps-logs or temps-analytics
        // For now, return basic data
        Ok(ErrorData {
            total_errors: 0,
            error_rate: 0.0,
            new_error_types: 0,
            most_common_errors: vec![],
            affected_users: 0,
            error_trend: vec![],
            uptime_percentage: 99.9,
            failed_health_checks: 0,
        })
    }

    /// Aggregate funnel and conversion data
    async fn aggregate_funnel_data(
        &self,
        __week_start: DateTime<Utc>,
        __week_end: DateTime<Utc>,
    ) -> Result<FunnelData> {
        // TODO: Implement funnel aggregation from temps-analytics-funnels
        Ok(FunnelData {
            total_funnels: 0,
            funnel_stats: vec![],
        })
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
        project_stats.sort_by(|a, b| b.visitors.cmp(&a.visitors));

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

    async fn setup_test_service() -> (DigestService, TestDatabase) {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        let encryption_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encryption_service = Arc::new(
            EncryptionService::new(encryption_key).expect("Failed to create encryption service"),
        );

        let notification_service = Arc::new(NotificationService::new(
            test_db.connection_arc(),
            encryption_service,
        ));

        let digest_service = DigestService::new(test_db.connection_arc(), notification_service);

        (digest_service, test_db)
    }

    #[tokio::test]
    async fn test_generate_weekly_digest_empty_data() {
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
        let (service, test_db) = setup_test_service().await;

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
}
