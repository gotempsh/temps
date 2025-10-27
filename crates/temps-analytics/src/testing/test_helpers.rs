#[cfg(test)]
use crate::{Analytics, AnalyticsService};
#[cfg(test)]
use rand;
#[cfg(test)]
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use temps_entities::{events, ip_geolocations, visitor};

/// Test helper for comprehensive analytics event testing
#[cfg(test)]
pub struct AnalyticsTestHelper {
    pub service: AnalyticsService,
    pub db: Arc<DatabaseConnection>,
    #[allow(dead_code)]
    test_database: temps_database::test_utils::TestDatabase,
}

#[cfg(test)]
impl AnalyticsTestHelper {
    /// Create a new test helper with unique database connection per instance
    pub async fn new() -> anyhow::Result<Self> {
        // Use a default test name if none provided
        Self::new_with_name("default_test").await
    }

    /// Create a new test helper with specified test name
    pub async fn new_with_name(_test_name: &str) -> anyhow::Result<Self> {
        let test_database = temps_database::test_utils::TestDatabase::with_migrations().await?;
        let db = test_database.db.clone();
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password(
            "test_password",
        ));
        let service = AnalyticsService::new(db.clone(), encryption_service);

        Ok(Self {
            service,
            db,
            test_database,
        })
    }

    /// Store comprehensive test analytics events
    pub async fn store_test_events(&self) -> anyhow::Result<TestDataSet> {
        // Clean any existing data
        self.cleanup().await?;

        // Create required parent records first
        use temps_entities::{deployments, environments, projects};

        // Insert test project
        let test_project = projects::ActiveModel {
            id: Set(1),
            name: Set("test_project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("test-project".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::Static),
            ..Default::default()
        };
        let _ = test_project.insert(self.db.as_ref()).await; // Ignore if exists

        // Insert test environment
        let test_environment = environments::ActiveModel {
            id: Set(1),
            name: Set("test".to_string()),
            project_id: Set(1),
            ..Default::default()
        };
        let _ = test_environment.insert(self.db.as_ref()).await; // Ignore if exists

        // Insert test deployment
        use temps_entities::deployments::DeploymentMetadata;
        let test_deployment = deployments::ActiveModel {
            id: Set(1),
            environment_id: Set(1),
            project_id: Set(1),
            state: Set("deployed".to_string()),
            slug: Set("test-deployment".to_string()),
            metadata: Set(Some(DeploymentMetadata {
                builder: Some("test".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        };
        let _ = test_deployment.insert(self.db.as_ref()).await; // Ignore if exists

        // Create test visitors
        let visitor1 = self
            .create_visitor("visitor_1", 1, "2024-01-01 10:00:00")
            .await?;
        let visitor2 = self
            .create_visitor("visitor_2", 1, "2024-01-01 11:00:00")
            .await?;
        let visitor3 = self
            .create_visitor("visitor_3", 1, "2024-01-01 12:00:00")
            .await?;

        // Create test geolocations
        let geo_us = self
            .create_geolocation("US", "California", "San Francisco", 37.7749, -122.4194)
            .await?;
        let geo_uk = self
            .create_geolocation("GB", "England", "London", 51.5074, -0.1278)
            .await?;
        let geo_ca = self
            .create_geolocation("CA", "Ontario", "Toronto", 43.6532, -79.3832)
            .await?;

        // Create comprehensive event dataset
        let events = vec![
            // Visitor 1 - Complete user journey
            TestEvent {
                visitor_id: visitor1.id,
                session_id: "session_1".to_string(),
                timestamp: "2024-01-01 10:00:00",
                event_type: "page_view".to_string(),
                page_path: "/".to_string(),
                referrer: Some("https://google.com".to_string()),
                browser: Some("Chrome".to_string()),
                os: Some("macOS".to_string()),
                geo_id: Some(geo_us.id),
                time_on_page: Some(45),
                is_crawler: false,
            },
            TestEvent {
                visitor_id: visitor1.id,
                session_id: "session_1".to_string(),
                timestamp: "2024-01-01 10:01:00",
                event_type: "page_view".to_string(),
                page_path: "/products".to_string(),
                referrer: None,
                browser: Some("Chrome".to_string()),
                os: Some("macOS".to_string()),
                geo_id: Some(geo_us.id),
                time_on_page: Some(120),
                is_crawler: false,
            },
            TestEvent {
                visitor_id: visitor1.id,
                session_id: "session_1".to_string(),
                timestamp: "2024-01-01 10:03:00",
                event_type: "custom".to_string(),
                page_path: "/products".to_string(),
                referrer: None,
                browser: Some("Chrome".to_string()),
                os: Some("macOS".to_string()),
                geo_id: Some(geo_us.id),
                time_on_page: None,
                is_crawler: false,
            },
            // Visitor 2 - Mobile user from UK
            TestEvent {
                visitor_id: visitor2.id,
                session_id: "session_2".to_string(),
                timestamp: "2024-01-01 11:00:00",
                event_type: "page_view".to_string(),
                page_path: "/".to_string(),
                referrer: Some("https://twitter.com".to_string()),
                browser: Some("Safari".to_string()),
                os: Some("iOS".to_string()),
                geo_id: Some(geo_uk.id),
                time_on_page: Some(30),
                is_crawler: false,
            },
            TestEvent {
                visitor_id: visitor2.id,
                session_id: "session_2".to_string(),
                timestamp: "2024-01-01 11:01:00",
                event_type: "page_view".to_string(),
                page_path: "/about".to_string(),
                referrer: None,
                browser: Some("Safari".to_string()),
                os: Some("iOS".to_string()),
                geo_id: Some(geo_uk.id),
                time_on_page: Some(60),
                is_crawler: false,
            },
            // Visitor 3 - Bot/Crawler
            TestEvent {
                visitor_id: visitor3.id,
                session_id: "session_3".to_string(),
                timestamp: "2024-01-01 12:00:00",
                event_type: "page_view".to_string(),
                page_path: "/sitemap.xml".to_string(),
                referrer: None,
                browser: Some("Googlebot".to_string()),
                os: Some("Linux".to_string()),
                geo_id: Some(geo_ca.id),
                time_on_page: Some(1),
                is_crawler: true,
            },
            // Additional events for stats
            TestEvent {
                visitor_id: visitor1.id,
                session_id: "session_1b".to_string(),
                timestamp: "2024-01-02 10:00:00",
                event_type: "page_view".to_string(),
                page_path: "/contact".to_string(),
                referrer: Some("https://facebook.com".to_string()),
                browser: Some("Firefox".to_string()),
                os: Some("Windows".to_string()),
                geo_id: Some(geo_us.id),
                time_on_page: Some(90),
                is_crawler: false,
            },
        ];

        let mut stored_events = Vec::new();
        for event in events {
            let stored = self.store_event(event).await?;
            stored_events.push(stored);
        }

        Ok(TestDataSet {
            visitors: vec![visitor1, visitor2, visitor3],
            geolocations: vec![geo_us, geo_uk, geo_ca],
            events: stored_events,
        })
    }

    /// Store a single analytics event
    async fn store_event(&self, event: TestEvent) -> anyhow::Result<events::Model> {
        let timestamp =
            chrono::DateTime::parse_from_rfc3339(event.timestamp)?.with_timezone(&chrono::Utc);

        let new_event = events::ActiveModel {
            visitor_id: Set(Some(event.visitor_id)),
            session_id: Set(Some(event.session_id)),
            project_id: Set(1),
            timestamp: Set(timestamp),
            event_type: Set(event.event_type),
            page_path: Set(event.page_path.clone()),
            referrer: Set(event.referrer),
            browser: Set(event.browser),
            operating_system: Set(event.os),
            ip_geolocation_id: Set(event.geo_id),
            time_on_page: Set(event.time_on_page),
            is_crawler: Set(event.is_crawler),
            environment_id: Set(Some(1)),
            deployment_id: Set(Some(1)),              // Add deployment_id
            hostname: Set("example.com".to_string()), // Add hostname
            pathname: Set(event.page_path.clone()),   // Add pathname (same as page_path)
            href: Set(format!("https://example.com{}", event.page_path)), // Add href
            ..Default::default()
        };

        Ok(new_event.insert(self.db.as_ref()).await?)
    }

    /// Create a test visitor
    async fn create_visitor(
        &self,
        visitor_id: &str,
        project_id: i32,
        first_seen: &str,
    ) -> anyhow::Result<visitor::Model> {
        let first_seen_dt =
            chrono::DateTime::parse_from_rfc3339(first_seen)?.with_timezone(&chrono::Utc);
        // Set last_seen to first_seen + 1 hour as a reasonable default
        let last_seen_dt = first_seen_dt + chrono::Duration::hours(1);

        let new_visitor = visitor::ActiveModel {
            visitor_id: Set(visitor_id.to_string()),
            project_id: Set(project_id),
            environment_id: Set(1), // Add environment_id
            first_seen: Set(first_seen_dt),
            last_seen: Set(last_seen_dt),
            custom_data: Set(None),
            ..Default::default()
        };

        Ok(new_visitor.insert(self.db.as_ref()).await?)
    }

    /// Create a test geolocation
    async fn create_geolocation(
        &self,
        country: &str,
        region: &str,
        city: &str,
        lat: f64,
        lon: f64,
    ) -> anyhow::Result<ip_geolocations::Model> {
        let new_geo = ip_geolocations::ActiveModel {
            ip_address: Set(format!("192.168.1.{}", rand::random::<u8>())), // Generate unique IP for testing
            country: Set(country.to_string()),
            country_code: Set(Some(country.to_string())),
            region: Set(Some(region.to_string())),
            city: Set(Some(city.to_string())),
            latitude: Set(Some(lat)),
            longitude: Set(Some(lon)),
            ..Default::default()
        };

        Ok(new_geo.insert(self.db.as_ref()).await?)
    }

    /// Verify analytics data by running various queries and checking results
    pub async fn verify_analytics_data(
        &self,
        dataset: &TestDataSet,
    ) -> anyhow::Result<VerificationResults> {
        let mut results = VerificationResults::default();

        // Test top pages (only currently implemented analytics query in test helpers)
        let top_pages = self.service.get_top_pages(1, 10, None, None).await?;
        results.top_pages_count = top_pages.len();
        results.has_top_pages = !top_pages.is_empty();

        // Test has analytics events
        let has_events = self.service.has_analytics_events(1, None).await?;
        results.has_analytics_events = has_events.has_events;

        // Verify data integrity
        results.data_integrity_checks = self.verify_data_integrity(dataset).await?;

        Ok(results)
    }

    /// Verify data integrity by checking counts and relationships
    async fn verify_data_integrity(
        &self,
        dataset: &TestDataSet,
    ) -> anyhow::Result<DataIntegrityChecks> {
        let mut checks = DataIntegrityChecks::default();

        // Check event count
        let total_events = events::Entity::find().count(self.db.as_ref()).await?;
        checks.total_events = total_events as usize;
        checks.expected_events = dataset.events.len();
        checks.event_count_matches = total_events as usize == dataset.events.len();

        // Check visitor count
        let total_visitors = visitor::Entity::find().count(self.db.as_ref()).await?;
        checks.total_visitors = total_visitors as usize;
        checks.expected_visitors = dataset.visitors.len();
        checks.visitor_count_matches = total_visitors as usize == dataset.visitors.len();

        // Check geolocation count
        let total_geos = ip_geolocations::Entity::find()
            .count(self.db.as_ref())
            .await?;
        checks.total_geolocations = total_geos as usize;
        checks.expected_geolocations = dataset.geolocations.len();
        checks.geolocation_count_matches = total_geos as usize == dataset.geolocations.len();

        // Check for crawlers vs real users
        let crawler_events = events::Entity::find()
            .filter(events::Column::IsCrawler.eq(true))
            .count(self.db.as_ref())
            .await?;
        checks.crawler_events = crawler_events as usize;

        let user_events = events::Entity::find()
            .filter(events::Column::IsCrawler.eq(false))
            .count(self.db.as_ref())
            .await?;
        checks.user_events = user_events as usize;

        Ok(checks)
    }

    /// Clean up test data
    pub async fn cleanup(&self) -> anyhow::Result<()> {
        crate::testing::test_utils::AnalyticsTestUtils::cleanup_test_data(&self.db).await?;
        Ok(())
    }
}

/// Test event structure for easy test data creation
#[cfg(test)]
#[derive(Clone)]
pub struct TestEvent {
    pub visitor_id: i32,
    pub session_id: String,
    pub timestamp: &'static str,
    pub event_type: String,
    pub page_path: String,
    pub referrer: Option<String>,
    pub browser: Option<String>,
    pub os: Option<String>,
    pub geo_id: Option<i32>,
    pub time_on_page: Option<i32>,
    pub is_crawler: bool,
}

/// Complete test dataset
#[cfg(test)]
pub struct TestDataSet {
    pub visitors: Vec<visitor::Model>,
    pub geolocations: Vec<ip_geolocations::Model>,
    pub events: Vec<events::Model>,
}

/// Results from verification tests
#[cfg(test)]
#[derive(Default, Debug)]
pub struct VerificationResults {
    pub top_pages_count: usize,
    pub has_top_pages: bool,
    pub has_analytics_events: bool,
    pub data_integrity_checks: DataIntegrityChecks,
}

/// Data integrity verification results
#[cfg(test)]
#[derive(Default, Debug)]
pub struct DataIntegrityChecks {
    pub total_events: usize,
    pub expected_events: usize,
    pub event_count_matches: bool,
    pub total_visitors: usize,
    pub expected_visitors: usize,
    pub visitor_count_matches: bool,
    pub total_geolocations: usize,
    pub expected_geolocations: usize,
    pub geolocation_count_matches: bool,
    pub crawler_events: usize,
    pub user_events: usize,
}

#[cfg(test)]
impl VerificationResults {
    /// Check if all verifications passed
    pub fn all_passed(&self) -> bool {
        self.has_top_pages
            && self.has_analytics_events
            && self.data_integrity_checks.event_count_matches
            && self.data_integrity_checks.visitor_count_matches
            && self.data_integrity_checks.geolocation_count_matches
    }

    /// Generate a summary report
    pub fn summary(&self) -> String {
        format!(
            r#"
📊 Analytics Verification Report
================================

📈 Data Availability:
  ✓ Top Pages: {} ({} pages)
  ✓ Has Events: {}

🔍 Data Integrity:
  ✓ Events: {}/{} {}
  ✓ Visitors: {}/{} {}
  ✓ Geolocations: {}/{} {}
  ✓ Crawler Events: {}
  ✓ User Events: {}

🎯 Overall Status: {}
            "#,
            self.has_top_pages,
            self.top_pages_count,
            self.has_analytics_events,
            self.data_integrity_checks.total_events,
            self.data_integrity_checks.expected_events,
            if self.data_integrity_checks.event_count_matches {
                "✅"
            } else {
                "❌"
            },
            self.data_integrity_checks.total_visitors,
            self.data_integrity_checks.expected_visitors,
            if self.data_integrity_checks.visitor_count_matches {
                "✅"
            } else {
                "❌"
            },
            self.data_integrity_checks.total_geolocations,
            self.data_integrity_checks.expected_geolocations,
            if self.data_integrity_checks.geolocation_count_matches {
                "✅"
            } else {
                "❌"
            },
            self.data_integrity_checks.crawler_events,
            self.data_integrity_checks.user_events,
            if self.all_passed() {
                "🎉 ALL TESTS PASSED"
            } else {
                "⚠️ SOME TESTS FAILED"
            }
        )
    }
}
