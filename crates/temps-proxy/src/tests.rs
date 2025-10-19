#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use temps_database::test_utils::TestDatabase;

    use crate::test_utils::*;
    use crate::traits::*;

    fn create_crypto_cookie_crypto() -> Arc<temps_core::CookieCrypto> {
        let encryption_key = "default-32-byte-key-for-testing!";
        Arc::new(temps_core::CookieCrypto::new(encryption_key).expect("Failed to create cookie crypto"))
    }
    #[tokio::test]
    async fn test_database_setup() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await.unwrap();

        // Test that migrations ran successfully by creating a simple record
        let (project, environment, deployment) = test_db.create_test_project().await?;

        assert!(project.name.contains("test-project")); // Project name now includes domain for uniqueness
        assert_eq!(environment.name, "production");
        assert_eq!(deployment.state, "running");

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_visitor_creation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await.unwrap();

        // Create test project
        let (project, _environment, _deployment) = test_db.create_test_project().await?;

        // Create test visitor
        let visitor = test_db.create_test_visitor(project.id).await?;
        assert_eq!(visitor.project_id, project.id);
        assert!(!visitor.is_crawler);

        // Convert to trait objects
        let visitor_trait = create_test_visitor_trait(visitor.clone());

        assert_eq!(visitor_trait.visitor_id_i32, visitor.id);
        assert_eq!(visitor_trait.is_crawler, visitor.is_crawler);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_cookie_config() -> Result<(), Box<dyn std::error::Error>> {
        let config = CookieConfig::default();

        assert_eq!(config.visitor_cookie_name, "_temps_visitor_id");
        assert_eq!(config.session_cookie_name, "_temps_sid");
        assert_eq!(config.visitor_max_age_days, 365);
        assert_eq!(config.session_max_age_minutes, 30);
        assert!(config.secure);
        assert!(config.http_only);
        assert_eq!(config.same_site, Some("Lax".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_service_error_types() -> Result<(), Box<dyn std::error::Error>> {
        // Test error type creation and display
        let error = ProxyServiceError::UpstreamResolution("Test error".to_string());
        assert!(error.to_string().contains("Upstream resolution failed"));

        let error = ProxyServiceError::RequestLogging("Log error".to_string());
        assert!(error.to_string().contains("Request logging failed"));

        let error = ProxyServiceError::ProjectContext("Context error".to_string());
        assert!(error.to_string().contains("Project context resolution failed"));

        let error = ProxyServiceError::Visitor("Visitor error".to_string());
        assert!(error.to_string().contains("Visitor management failed"));

        let error = ProxyServiceError::Session("Session error".to_string());
        assert!(error.to_string().contains("Session management failed"));

        Ok(())
    }

    #[tokio::test]
    async fn test_server_config() -> Result<(), Box<dyn std::error::Error>> {
        let config = crate::config::ProxyConfig::default();

        assert_eq!(config.address, "127.0.0.1:8080");
        assert_eq!(config.console_address, "127.0.0.1:3000");
        assert_eq!(config.tls_address, None);

        Ok(())
    }

    #[tokio::test]
    async fn test_cookie_crypto() -> Result<(), Box<dyn std::error::Error>> {
        let crypto = create_crypto_cookie_crypto();

        let original = "test_data";
        let encrypted = crypto.encrypt(original)?;
        let decrypted = crypto.decrypt(&encrypted)?;

        assert_eq!(original, decrypted);

        Ok(())
    }

    #[tokio::test]
    async fn test_project_context_creation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await.unwrap();

        // Create test project
        let (project, environment, deployment) = test_db.create_test_project().await?;
        let project_context = create_test_project_context(project.clone(), environment.clone(), deployment.clone());

        assert_eq!(project_context.project.id, project.id);
        assert_eq!(project_context.environment.id, environment.id);
        assert_eq!(project_context.deployment.id, deployment.id);

        test_db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_custom_route_creation() -> Result<(), Box<dyn std::error::Error>> {
        let test_db_mock = TestDatabase::with_migrations().await.unwrap();
        let test_db = TestDBMockOperations::new(test_db_mock.db.clone()).await.unwrap();

        // Create test custom route
        let custom_route = test_db.create_test_custom_route("custom.example.com").await?;
        assert_eq!(custom_route.domain, "custom.example.com");
        assert!(custom_route.enabled);

        test_db.cleanup().await?;
        Ok(())
    }
}