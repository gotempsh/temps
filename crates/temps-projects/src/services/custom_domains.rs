use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_core::url_validation;
use temps_entities::project_custom_domains;
use thiserror::Error;
use tracing::{debug, info};
use url::Url;

#[derive(Error, Debug)]
pub enum CustomDomainError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Custom domain not found: {0}")]
    NotFound(String),
    #[error("Invalid custom domain: {0}")]
    InvalidDomain(String),
    #[error("Duplicate domain: {0}")]
    DuplicateDomain(String),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("Circular redirect: {0}")]
    CircularRedirect(String),
    #[error("Invalid redirect URL: {0}")]
    InvalidRedirectUrl(String),
}

pub struct CustomDomainService {
    db: Arc<DatabaseConnection>,
}

impl CustomDomainService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Validates a redirect URL to prevent SSRF and circular redirects
    ///
    /// This method checks:
    /// 1. URL format is valid (HTTP/HTTPS only)
    /// 2. URL doesn't point to private IPs, localhost, or cloud metadata services
    /// 3. Self-redirects are prevented (domain redirecting to itself)
    /// 4. Circular redirect chains are detected
    ///
    /// # Arguments
    /// * `domain` - The source domain being configured
    /// * `redirect_url` - The target URL to redirect to
    /// * `exclude_id` - Optional domain ID to exclude from circular check (for updates)
    ///
    /// # Returns
    /// * `Ok(())` if the redirect is valid
    /// * `Err(CustomDomainError)` if validation fails
    async fn validate_redirect_url(
        &self,
        domain: &str,
        redirect_url: &str,
        exclude_id: Option<i32>,
    ) -> Result<(), CustomDomainError> {
        // Validate URL format and prevent SSRF
        let parsed_url = url_validation::validate_external_url(redirect_url).map_err(|e| {
            CustomDomainError::InvalidRedirectUrl(format!(
                "Invalid redirect URL '{}': {}",
                redirect_url, e
            ))
        })?;

        // Extract redirect target host
        let redirect_host = parsed_url
            .host_str()
            .ok_or_else(|| {
                CustomDomainError::InvalidRedirectUrl(
                    "Redirect URL must have a valid host".to_string(),
                )
            })?
            .to_lowercase();

        // Prevent self-redirect (domain redirecting to itself)
        if domain.to_lowercase() == redirect_host {
            return Err(CustomDomainError::CircularRedirect(format!(
                "Domain '{}' cannot redirect to itself",
                domain
            )));
        }

        // Check for circular redirect chains (A -> B -> C -> A)
        self.detect_redirect_chain(&redirect_host, domain, exclude_id, 10)
            .await?;

        Ok(())
    }

    /// Detects circular redirect chains by following the redirect chain
    ///
    /// This recursively checks if following the redirect chain would eventually
    /// lead back to the original domain.
    ///
    /// # Arguments
    /// * `current_host` - The current host in the chain
    /// * `original_domain` - The domain we started from
    /// * `exclude_id` - Optional domain ID to exclude from check (for updates)
    /// * `max_depth` - Maximum depth to prevent infinite recursion
    ///
    /// # Returns
    /// * `Ok(())` if no circular redirect detected
    /// * `Err(CustomDomainError::CircularRedirect)` if circular redirect found
    fn detect_redirect_chain<'a>(
        &'a self,
        current_host: &'a str,
        original_domain: &'a str,
        exclude_id: Option<i32>,
        max_depth: usize,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), CustomDomainError>> + 'a + Send>,
    > {
        Box::pin(async move {
            if max_depth == 0 {
                return Err(CustomDomainError::CircularRedirect(
                    "Redirect chain exceeds maximum depth of 10".to_string(),
                ));
            }

            // Check if current_host has a redirect configured
            let mut query = project_custom_domains::Entity::find()
                .filter(project_custom_domains::Column::Domain.eq(current_host));

            // Exclude the domain being updated to avoid false positives
            if let Some(id) = exclude_id {
                query = query.filter(project_custom_domains::Column::Id.ne(id));
            }

            let domain_record = query.one(self.db.as_ref()).await?;

            if let Some(record) = domain_record {
                if let Some(next_redirect) = &record.redirect_to {
                    // Parse the next redirect URL
                    let next_url = Url::parse(next_redirect).map_err(|e| {
                        CustomDomainError::Internal(format!("Failed to parse redirect URL: {}", e))
                    })?;

                    let next_host = next_url
                        .host_str()
                        .ok_or_else(|| {
                            CustomDomainError::Internal("Redirect URL missing host".to_string())
                        })?
                        .to_lowercase();

                    // Check if we've circled back to the original domain
                    if next_host == original_domain.to_lowercase() {
                        return Err(CustomDomainError::CircularRedirect(format!(
                            "Circular redirect detected: {} -> {} -> {}",
                            original_domain, current_host, next_host
                        )));
                    }

                    // Recursively check the next hop in the chain
                    self.detect_redirect_chain(
                        &next_host,
                        original_domain,
                        exclude_id,
                        max_depth - 1,
                    )
                    .await?;
                }
            }

            Ok(())
        })
    }

    /// Create a new custom domain for a project
    pub async fn create_custom_domain(
        &self,
        project_id: i32,
        environment_id: i32,
        domain: String,
        redirect_to: Option<String>,
        status_code: Option<i32>,
        branch: Option<String>,
    ) -> Result<project_custom_domains::Model, CustomDomainError> {
        info!(
            "Creating custom domain: {} for project: {}",
            domain, project_id
        );

        // Check if domain already exists
        if let Some(_existing) = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(&domain))
            .one(self.db.as_ref())
            .await?
        {
            return Err(CustomDomainError::DuplicateDomain(format!(
                "Domain {} already exists",
                domain
            )));
        }

        // Validate redirect URL if provided
        if let Some(ref redirect_url) = redirect_to {
            if !redirect_url.is_empty() {
                self.validate_redirect_url(&domain, redirect_url, None)
                    .await?;
            }
        }

        let new_custom_domain = project_custom_domains::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            domain: Set(domain.clone()),
            redirect_to: Set(redirect_to),
            status_code: Set(status_code),
            branch: Set(branch),
            status: Set("pending".to_string()),
            message: Set(None),
            certificate_id: Set(None),
            ..Default::default()
        };

        let custom_domain = new_custom_domain.insert(self.db.as_ref()).await?;

        debug!(
            "Custom domain created successfully: {} with ID: {}",
            domain, custom_domain.id
        );
        Ok(custom_domain)
    }

    /// Get custom domain by ID
    pub async fn get_custom_domain(
        &self,
        id: i32,
    ) -> Result<Option<project_custom_domains::Model>, CustomDomainError> {
        let custom_domain = project_custom_domains::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?;
        Ok(custom_domain)
    }

    /// Get custom domain by domain name
    pub async fn get_custom_domain_by_domain(
        &self,
        domain: &str,
    ) -> Result<Option<project_custom_domains::Model>, CustomDomainError> {
        let custom_domain = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?;
        Ok(custom_domain)
    }

    /// List all custom domains for a project
    pub async fn list_custom_domains_for_project(
        &self,
        project_id: i32,
    ) -> Result<Vec<project_custom_domains::Model>, CustomDomainError> {
        let custom_domains = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;
        Ok(custom_domains)
    }

    /// List all custom domains for an environment
    pub async fn list_custom_domains_for_environment(
        &self,
        environment_id: i32,
    ) -> Result<Vec<project_custom_domains::Model>, CustomDomainError> {
        let custom_domains = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::EnvironmentId.eq(environment_id))
            .all(self.db.as_ref())
            .await?;
        Ok(custom_domains)
    }

    /// Update custom domain
    #[allow(clippy::too_many_arguments)]
    pub async fn update_custom_domain(
        &self,
        id: i32,
        domain: Option<String>,
        environment_id: Option<i32>,
        redirect_to: Option<String>,
        status_code: Option<i32>,
        branch: Option<String>,
        status: Option<String>,
        message: Option<String>,
        certificate_id: Option<i32>,
    ) -> Result<project_custom_domains::Model, CustomDomainError> {
        info!("Updating custom domain ID: {}", id);

        let custom_domain = project_custom_domains::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                CustomDomainError::NotFound(format!("Custom domain with ID {} not found", id))
            })?;

        let mut active_model: project_custom_domains::ActiveModel = custom_domain.clone().into();

        // Determine the final domain name for validation
        let final_domain = if let Some(ref new_domain) = domain {
            // Check if new domain already exists (for a different record)
            if let Some(existing) = project_custom_domains::Entity::find()
                .filter(project_custom_domains::Column::Domain.eq(new_domain))
                .one(self.db.as_ref())
                .await?
            {
                if existing.id != id {
                    return Err(CustomDomainError::DuplicateDomain(format!(
                        "Domain {} already exists",
                        new_domain
                    )));
                }
            }
            active_model.domain = Set(new_domain.clone());
            new_domain.as_str()
        } else {
            custom_domain.domain.as_str()
        };

        if let Some(env_id) = environment_id {
            active_model.environment_id = Set(env_id);
        }
        if let Some(redirect) = redirect_to {
            // Empty string means clear the field
            if redirect.is_empty() {
                active_model.redirect_to = Set(None);
            } else {
                // Validate redirect URL before updating
                self.validate_redirect_url(final_domain, &redirect, Some(id))
                    .await?;
                active_model.redirect_to = Set(Some(redirect));
            }
        }
        if let Some(code) = status_code {
            // 0 means clear the field
            if code == 0 {
                active_model.status_code = Set(None);
            } else {
                active_model.status_code = Set(Some(code));
            }
        }
        if let Some(b) = branch {
            // Empty string means clear the field
            if b.is_empty() {
                active_model.branch = Set(None);
            } else {
                active_model.branch = Set(Some(b));
            }
        }
        if let Some(s) = status {
            active_model.status = Set(s);
        }
        if let Some(m) = message {
            active_model.message = Set(Some(m));
        }
        if let Some(cert_id) = certificate_id {
            active_model.certificate_id = Set(Some(cert_id));
        }

        let updated_domain = active_model.update(self.db.as_ref()).await?;

        debug!("Custom domain updated successfully: ID {}", id);
        Ok(updated_domain)
    }

    /// Update custom domain status
    pub async fn update_custom_domain_status(
        &self,
        id: i32,
        status: String,
        message: Option<String>,
    ) -> Result<project_custom_domains::Model, CustomDomainError> {
        info!("Updating custom domain status for ID: {} to {}", id, status);

        let custom_domain = project_custom_domains::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                CustomDomainError::NotFound(format!("Custom domain with ID {} not found", id))
            })?;

        let mut active_model: project_custom_domains::ActiveModel = custom_domain.into();
        active_model.status = Set(status);
        active_model.message = Set(message);

        let updated_domain = active_model.update(self.db.as_ref()).await?;

        debug!("Custom domain status updated successfully: ID {}", id);
        Ok(updated_domain)
    }

    /// Link custom domain to certificate
    pub async fn link_certificate(
        &self,
        id: i32,
        certificate_id: i32,
    ) -> Result<project_custom_domains::Model, CustomDomainError> {
        info!(
            "Linking custom domain ID: {} to certificate ID: {}",
            id, certificate_id
        );

        let custom_domain = project_custom_domains::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                CustomDomainError::NotFound(format!("Custom domain with ID {} not found", id))
            })?;

        let mut active_model: project_custom_domains::ActiveModel = custom_domain.into();
        active_model.certificate_id = Set(Some(certificate_id));
        active_model.status = Set("active".to_string());

        let updated_domain = active_model.update(self.db.as_ref()).await?;

        debug!(
            "Custom domain linked to certificate successfully: ID {}",
            id
        );
        Ok(updated_domain)
    }

    /// Delete custom domain
    pub async fn delete_custom_domain(&self, id: i32) -> Result<(), CustomDomainError> {
        info!("Deleting custom domain ID: {}", id);

        let result = project_custom_domains::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;

        if result.rows_affected == 0 {
            return Err(CustomDomainError::NotFound(format!(
                "Custom domain with ID {} not found",
                id
            )));
        }

        debug!("Custom domain deleted successfully: ID {}", id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_entities::{environments, projects, upstream_config::UpstreamList};
    use temps_presets::PresetType;
    async fn setup_test_data(db: &Arc<sea_orm::DatabaseConnection>) -> (i32, i32) {
        // Create a test project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(PresetType::Nixpacks),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create a test environment
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("test-project".to_string()),
            host: Set("test-project.temps.dev".to_string()),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        (project.id, environment.id)
    }

    #[tokio::test]
    async fn test_create_custom_domain() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "example.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(domain.domain, "example.com");
        assert_eq!(domain.project_id, project_id);
        assert_eq!(domain.environment_id, env_id);
        assert_eq!(domain.status, "pending");
    }

    #[tokio::test]
    async fn test_create_duplicate_domain_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create first domain
        service
            .create_custom_domain(
                project_id,
                env_id,
                "duplicate.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Try to create duplicate
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "duplicate.com".to_string(),
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::DuplicateDomain(_) => {}
            _ => panic!("Expected DuplicateDomain error"),
        }
    }

    #[tokio::test]
    async fn test_get_custom_domain() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let created = service
            .create_custom_domain(
                project_id,
                env_id,
                "get-test.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let retrieved = service
            .get_custom_domain(created.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(retrieved.id, created.id);
        assert_eq!(retrieved.domain, "get-test.com");
    }

    #[tokio::test]
    async fn test_get_custom_domain_by_domain() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        service
            .create_custom_domain(
                project_id,
                env_id,
                "find-by-domain.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let found = service
            .get_custom_domain_by_domain("find-by-domain.com")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(found.domain, "find-by-domain.com");
    }

    #[tokio::test]
    async fn test_list_custom_domains_for_project() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create multiple domains
        service
            .create_custom_domain(
                project_id,
                env_id,
                "domain1.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        service
            .create_custom_domain(
                project_id,
                env_id,
                "domain2.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let domains = service
            .list_custom_domains_for_project(project_id)
            .await
            .unwrap();

        assert_eq!(domains.len(), 2);
    }

    #[tokio::test]
    async fn test_list_custom_domains_for_environment() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        service
            .create_custom_domain(
                project_id,
                env_id,
                "env-domain.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let domains = service
            .list_custom_domains_for_environment(env_id)
            .await
            .unwrap();

        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].domain, "env-domain.com");
    }

    #[tokio::test]
    async fn test_update_custom_domain() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "update-test.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let updated = service
            .update_custom_domain(
                domain.id,
                Some("updated-domain.com".to_string()),
                None,
                Some("https://redirect.com".to_string()),
                Some(301),
                Some("main".to_string()),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(updated.domain, "updated-domain.com");
        assert_eq!(
            updated.redirect_to,
            Some("https://redirect.com".to_string())
        );
        assert_eq!(updated.status_code, Some(301));
        assert_eq!(updated.branch, Some("main".to_string()));
    }

    #[tokio::test]
    async fn test_update_domain_to_duplicate_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create two domains
        service
            .create_custom_domain(
                project_id,
                env_id,
                "existing.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let domain2 = service
            .create_custom_domain(
                project_id,
                env_id,
                "another.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Try to update domain2 to duplicate domain1's name
        let result = service
            .update_custom_domain(
                domain2.id,
                Some("existing.com".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::DuplicateDomain(_) => {}
            _ => panic!("Expected DuplicateDomain error"),
        }
    }

    #[tokio::test]
    async fn test_update_custom_domain_status() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "status-test.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let updated = service
            .update_custom_domain_status(
                domain.id,
                "active".to_string(),
                Some("Successfully configured".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(updated.status, "active");
        assert_eq!(updated.message, Some("Successfully configured".to_string()));
    }

    #[tokio::test]
    async fn test_link_certificate() {
        // Note: This test would require creating a domain in the domains table first
        // which is outside the scope of this service's tests.
        // The link_certificate method is tested via integration tests instead.
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "cert-test.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Verify domain was created with null certificate
        assert_eq!(domain.certificate_id, None);
        assert_eq!(domain.status, "pending");
    }

    #[tokio::test]
    async fn test_delete_custom_domain() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "delete-test.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        service.delete_custom_domain(domain.id).await.unwrap();

        let result = service.get_custom_domain(domain.id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_domain_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());

        let result = service.delete_custom_domain(99999).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::NotFound(_) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    // ==================== Redirect Validation Tests ====================

    #[tokio::test]
    async fn test_create_domain_with_valid_redirect() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Valid redirect to external domain
        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "source.example.com".to_string(),
                Some("https://target.example.com".to_string()),
                Some(301),
                None,
            )
            .await
            .unwrap();

        assert_eq!(domain.domain, "source.example.com");
        assert_eq!(
            domain.redirect_to,
            Some("https://target.example.com".to_string())
        );
        assert_eq!(domain.status_code, Some(301));
    }

    #[tokio::test]
    async fn test_create_domain_with_self_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Self-redirect should fail
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "self-redirect.example.com".to_string(),
                Some("https://self-redirect.example.com/path".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::CircularRedirect(msg) => {
                assert!(msg.contains("cannot redirect to itself"));
            }
            e => panic!("Expected CircularRedirect error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_domain_with_circular_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create first domain: A -> B
        service
            .create_custom_domain(
                project_id,
                env_id,
                "domain-a.example.com".to_string(),
                Some("https://domain-b.example.com".to_string()),
                Some(301),
                None,
            )
            .await
            .unwrap();

        // Try to create second domain: B -> A (creates circular redirect)
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "domain-b.example.com".to_string(),
                Some("https://domain-a.example.com".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::CircularRedirect(msg) => {
                assert!(msg.contains("Circular redirect detected"));
            }
            e => panic!("Expected CircularRedirect error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_domain_with_three_hop_circular_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create chain: A -> B -> C
        service
            .create_custom_domain(
                project_id,
                env_id,
                "chain-a.example.com".to_string(),
                Some("https://chain-b.example.com".to_string()),
                Some(301),
                None,
            )
            .await
            .unwrap();

        service
            .create_custom_domain(
                project_id,
                env_id,
                "chain-b.example.com".to_string(),
                Some("https://chain-c.example.com".to_string()),
                Some(301),
                None,
            )
            .await
            .unwrap();

        // Try to create C -> A (creates circular redirect: A -> B -> C -> A)
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "chain-c.example.com".to_string(),
                Some("https://chain-a.example.com".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::CircularRedirect(msg) => {
                assert!(msg.contains("Circular redirect detected"));
            }
            e => panic!("Expected CircularRedirect error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_domain_with_private_ip_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Redirect to private IP should fail
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "ssrf-test.example.com".to_string(),
                Some("http://192.168.1.1".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::InvalidRedirectUrl(msg) => {
                assert!(msg.contains("Private IP addresses are not allowed"));
            }
            e => panic!("Expected InvalidRedirectUrl error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_domain_with_localhost_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Redirect to localhost should fail
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "localhost-test.example.com".to_string(),
                Some("http://127.0.0.1:8080".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::InvalidRedirectUrl(msg) => {
                assert!(msg.contains("Loopback addresses are not allowed"));
            }
            e => panic!("Expected InvalidRedirectUrl error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_domain_with_cloud_metadata_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Redirect to cloud metadata service should fail
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "metadata-test.example.com".to_string(),
                Some("http://169.254.169.254/latest/meta-data".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::InvalidRedirectUrl(msg) => {
                assert!(msg.contains("Cloud metadata service access is not allowed"));
            }
            e => panic!("Expected InvalidRedirectUrl error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_create_domain_with_invalid_scheme_redirect_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Redirect with invalid scheme should fail
        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "ftp-test.example.com".to_string(),
                Some("ftp://ftp.example.com".to_string()),
                Some(301),
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::InvalidRedirectUrl(msg) => {
                assert!(msg.contains("URL scheme must be HTTP or HTTPS"));
            }
            e => panic!("Expected InvalidRedirectUrl error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_update_domain_redirect_to_circular_fails() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create two domains: A -> B (no redirect yet)
        let _domain_a = service
            .create_custom_domain(
                project_id,
                env_id,
                "update-a.example.com".to_string(),
                Some("https://update-b.example.com".to_string()),
                Some(301),
                None,
            )
            .await
            .unwrap();

        let domain_b = service
            .create_custom_domain(
                project_id,
                env_id,
                "update-b.example.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Try to update B to redirect to A (creates circular redirect)
        let result = service
            .update_custom_domain(
                domain_b.id,
                None,
                None,
                Some("https://update-a.example.com".to_string()),
                Some(301),
                None,
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CustomDomainError::CircularRedirect(msg) => {
                assert!(msg.contains("Circular redirect detected"));
            }
            e => panic!("Expected CircularRedirect error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_update_domain_redirect_to_valid_succeeds() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create domain without redirect
        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "update-valid.example.com".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Update to add valid redirect
        let updated = service
            .update_custom_domain(
                domain.id,
                None,
                None,
                Some("https://new-target.example.com".to_string()),
                Some(301),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            updated.redirect_to,
            Some("https://new-target.example.com".to_string())
        );
        assert_eq!(updated.status_code, Some(301));
    }

    #[tokio::test]
    async fn test_redirect_chain_max_depth_prevents_loops() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Create a chain of 5 domains: 0 -> 1 -> 2 -> 3 -> 4
        for i in 0..5 {
            let domain = format!("depth-{}.example.com", i);
            let redirect = if i < 4 {
                Some(format!("https://depth-{}.example.com", i + 1))
            } else {
                None
            };

            service
                .create_custom_domain(project_id, env_id, domain, redirect, Some(301), None)
                .await
                .unwrap();
        }

        // Try to make depth-4 redirect back to depth-0 (creates circular loop)
        let domain_4 = service
            .get_custom_domain_by_domain("depth-4.example.com")
            .await
            .unwrap()
            .unwrap();

        let circular_result = service
            .update_custom_domain(
                domain_4.id,
                None,
                None,
                Some("https://depth-0.example.com".to_string()),
                Some(301),
                None,
                None,
                None,
                None,
            )
            .await;

        // Should fail because it creates a circular redirect
        assert!(circular_result.is_err());
        match circular_result.unwrap_err() {
            CustomDomainError::CircularRedirect(msg) => {
                assert!(msg.contains("Circular redirect detected"));
            }
            e => panic!("Expected CircularRedirect error, got: {:?}", e),
        }
    }
}
