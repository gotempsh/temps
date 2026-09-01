// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{Database, DatabaseConnection};
#[cfg(test)]
use std::sync::Arc;
use temps_entities::upstream_config::UpstreamList;
use temps_migrations::{Migrator, MigratorTrait};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};
use uuid::Uuid;

/// Attempts made by [`connect_with_retries`]; see its call site for why this is
/// only a backstop rather than the primary readiness mechanism.
const DB_CONNECT_ATTEMPTS: u32 = 15;
const DB_CONNECT_RETRY_DELAY: tokio::time::Duration = tokio::time::Duration::from_secs(2);

async fn connect_with_retries(database_url: &str) -> anyhow::Result<DatabaseConnection> {
    let mut last_err = None;
    for _ in 0..DB_CONNECT_ATTEMPTS {
        match Database::connect(database_url).await {
            Ok(db) => return Ok(db),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(DB_CONNECT_RETRY_DELAY).await;
            }
        }
    }
    Err(anyhow::anyhow!(
        "Failed to connect to database after {DB_CONNECT_ATTEMPTS} attempts: {}",
        last_err.expect("at least one attempt was made")
    ))
}

/// Test database setup with unique container per test
pub struct TestDatabase {
    #[allow(dead_code)]
    container: ContainerAsync<GenericImage>,
    pub db: Arc<DatabaseConnection>,
    pub database_url: String,
}

impl TestDatabase {
    /// Create a new test database with unique TimescaleDB container
    pub async fn new(test_name: &str) -> anyhow::Result<Self> {
        // Create unique database name for this specific test (not used, but could be used for future isolation)
        let _unique_db_name = format!(
            "test_db_{}_{}",
            test_name,
            Uuid::new_v4().to_string().replace('-', "")
        );

        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
            // Without a readiness condition, `start()` returns as soon as the
            // container is running — long before PostgreSQL accepts clients —
            // and the fixed sleep that used to stand in for it lost the race on
            // loaded CI runners, failing tests with `error communicating with
            // database: Connection reset by peer (os error 104)`.
            //
            // The stream matters. The `timescaledb-ha` entrypoint boots a
            // *temporary* server for initdb, stops it, then starts the real
            // one, and each logs this line once. Verified against
            // `timescale/timescaledb-ha:pg18`, the temporary server logs to
            // **stdout** and the real server to **stderr**, so matching on
            // stderr targets the real server. Do not "fix" this by matching
            // stdout or by waiting for two occurrences — each stream only ever
            // carries one.
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_env_var("POSTGRES_DB", "test_db")
            .with_env_var("POSTGRES_USER", "test_user")
            .with_env_var("POSTGRES_PASSWORD", "test_password")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            // The background-worker launcher polls independently of the test
            // and is pure noise here (it floods the log with "out of background
            // workers" and can compress chunks mid-test).
            .with_cmd(vec![
                "postgres",
                "-c",
                "timescaledb.max_background_workers=0",
            ])
            // testcontainers' default is 60s; this repository runs many
            // Docker-backed tests concurrently, so give real headroom under
            // contention instead of racing the default.
            .with_startup_timeout(std::time::Duration::from_secs(120))
            .start()
            .await?;

        // Get connection details
        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!(
            "postgresql://test_user:test_password@localhost:{}/test_db",
            port
        );

        // Backstop only — the wait condition above is what removes the race.
        // This covers the brief window between the server logging readiness and
        // the mapped port accepting connections.
        let db = connect_with_retries(&database_url).await?;

        // Run migrations
        Migrator::up(&db, None).await?;

        Ok(TestDatabase {
            container: postgres_container,
            db: Arc::new(db),
            database_url,
        })
    }
}

/// Test utilities for analytics testing
pub struct AnalyticsTestUtils;

impl AnalyticsTestUtils {
    /// Create a test database connection with migrations - each test gets fresh DB
    pub async fn create_test_db(test_name: &str) -> anyhow::Result<Arc<DatabaseConnection>> {
        let test_db = TestDatabase::new(test_name).await?;
        Ok(test_db.db)
    }

    /// Insert test data for analytics testing
    pub async fn insert_test_data(db: &DatabaseConnection) -> anyhow::Result<()> {
        use chrono::{TimeZone, Utc};
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::{
            deployments, environments, events, ip_geolocations, projects, visitor,
        };

        // Create required parent records first

        // Insert test project
        let test_project = projects::ActiveModel {
            name: Set("test_project".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set("test_project".to_string()),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            created_at: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            updated_at: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            // Fill Option fields with None or Some as appropriate
            repo_name: Set("test_project".to_string()),
            repo_owner: Set("test_project".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            deleted_at: Set(None),
            last_deployment: Set(None),
            ..Default::default()
        };
        let test_project = test_project.insert(db).await?; // Ignore if exists

        // Insert test environment
        let test_environment = environments::ActiveModel {
            name: Set("test".to_string()),
            slug: Set("test-environment".to_string()),
            subdomain: Set("https://test-environment.example.com".to_string()),
            last_deployment: Set(None),
            host: Set("test-environment.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            updated_at: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            project_id: Set(test_project.id),
            current_deployment_id: Set(None),
            branch: Set(Some("main".to_string())),
            ..Default::default()
        };
        let test_environment = test_environment.insert(db).await?; // Ignore if exists

        // Insert test deployment
        use temps_entities::deployments::DeploymentMetadata;
        let test_deployment = deployments::ActiveModel {
            environment_id: Set(test_environment.id),
            project_id: Set(test_project.id),
            state: Set("deployed".to_string()),
            slug: Set("https://deployment.example.com".to_string()),
            created_at: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            updated_at: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            metadata: Set(Some(DeploymentMetadata {
                builder: Some("test".to_string()),
                ..Default::default()
            })),
            deploying_at: Set(None),
            ready_at: Set(None),
            static_dir_location: Set(None),
            screenshot_location: Set(None),
            image_name: Set(None),
            ..Default::default()
        };
        let test_deployment = test_deployment.insert(db).await?; // Ignore if exists

        // Insert test visitor
        let test_visitor = visitor::ActiveModel {
            visitor_id: Set("test_visitor_1".to_string()),
            project_id: Set(test_project.id),
            environment_id: Set(test_environment.id), // Add environment_id
            first_seen: Set(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            last_seen: Set(Utc.with_ymd_and_hms(2024, 1, 1, 10, 1, 0).unwrap()),
            custom_data: Set(None),
            ..Default::default()
        };
        let visitor_result = test_visitor.insert(db).await?;

        // Insert test IP geolocation
        let test_geolocation = ip_geolocations::ActiveModel {
            ip_address: Set("192.168.1.1".to_string()), // Add required ip_address
            country: Set("US".to_string()),
            country_code: Set(Some("US".to_string())),
            region: Set(Some("California".to_string())),
            city: Set(Some("San Francisco".to_string())),
            latitude: Set(Some(37.7749)),
            longitude: Set(Some(-122.4194)),
            ..Default::default()
        };
        let geo_result = test_geolocation.insert(db).await?;

        // Insert test events
        let test_events = vec![
            events::ActiveModel {
                visitor_id: Set(Some(visitor_result.id)),
                session_id: Set(Some("session_1".to_string())),
                project_id: Set(test_project.id),
                environment_id: Set(Some(test_environment.id)), // Add environment_id
                deployment_id: Set(Some(test_deployment.id)),   // Add deployment_id
                hostname: Set("example.com".to_string()),       // Add hostname
                pathname: Set("/home".to_string()), // Add pathname (same as page_path for now)
                href: Set("https://example.com/home".to_string()), // Add href
                timestamp: Set(Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap()),
                event_type: Set("page_view".to_string()),
                page_path: Set("/home".to_string()),
                referrer: Set(Some("https://google.com".to_string())),
                browser: Set(Some("Chrome".to_string())),
                operating_system: Set(Some("macOS".to_string())),
                ip_geolocation_id: Set(Some(geo_result.id)),
                time_on_page: Set(Some(60)),
                is_crawler: Set(false),
                ..Default::default()
            },
            events::ActiveModel {
                visitor_id: Set(Some(visitor_result.id)),
                session_id: Set(Some("session_1".to_string())),
                project_id: Set(1),
                environment_id: Set(Some(1)), // Add environment_id
                deployment_id: Set(Some(1)),  // Add deployment_id
                hostname: Set("example.com".to_string()), // Add hostname
                pathname: Set("/about".to_string()), // Add pathname (same as page_path for now)
                href: Set("https://example.com/about".to_string()), // Add href
                timestamp: Set(Utc.with_ymd_and_hms(2024, 1, 1, 10, 1, 0).unwrap()),
                event_type: Set("page_view".to_string()),
                page_path: Set("/about".to_string()),
                referrer: Set(None),
                browser: Set(Some("Chrome".to_string())),
                operating_system: Set(Some("macOS".to_string())),
                ip_geolocation_id: Set(Some(geo_result.id)),
                time_on_page: Set(Some(45)),
                is_crawler: Set(false),
                ..Default::default()
            },
        ];

        for event in test_events {
            event.insert(db).await?;
        }

        Ok(())
    }

    /// Clean up test data
    pub async fn cleanup_test_data(db: &DatabaseConnection) -> anyhow::Result<()> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::{events, ip_geolocations, visitor};

        // Delete test events
        events::Entity::delete_many()
            .filter(events::Column::ProjectId.eq(1))
            .exec(db)
            .await?;

        // Delete test visitors
        visitor::Entity::delete_many()
            .filter(visitor::Column::ProjectId.eq(1))
            .exec(db)
            .await?;

        // Delete test geolocations (clean up by country code)
        ip_geolocations::Entity::delete_many()
            .filter(ip_geolocations::Column::CountryCode.eq("US"))
            .exec(db)
            .await?;

        Ok(())
    }
}

/// Macro to create analytics service for testing
#[macro_export]
macro_rules! create_test_analytics_service {
    ($test_name:expr) => {{
        let test_db = $crate::testing::test_utils::TestDatabase::new($test_name)
            .await
            .unwrap();
        $crate::testing::test_utils::AnalyticsTestUtils::insert_test_data(&test_db.db)
            .await
            .unwrap();
        let cookie_crypto =
            Arc::new(temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap());
        let service = $crate::AnalyticsService::new(test_db.db.clone(), cookie_crypto);
        (service, test_db.db.clone(), test_db) // Return the TestDatabase to keep container alive
    }};
}

/// Macro for test cleanup
#[macro_export]
macro_rules! cleanup_test_analytics {
    ($db:expr) => {{
        $crate::testing::test_utils::AnalyticsTestUtils::cleanup_test_data(&$db)
            .await
            .unwrap();
    }};
}
