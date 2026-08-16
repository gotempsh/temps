use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, QueryFilter, QuerySelect, Set, TransactionTrait,
};
use std::sync::Arc;
use temps_core::url_validation;
use temps_core::AppSettings;
use temps_entities::{domains, environments, project_custom_domains, settings};
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
    #[error(
        "Custom domain {domain_id} is no longer assigned to source project {source_project_id}"
    )]
    AssignmentChanged {
        domain_id: i32,
        source_project_id: i32,
    },
    #[error(
        "Failed to persist audit intent before reassigning custom domain {domain_id} from project {source_project_id} to project {target_project_id}: {reason}"
    )]
    AuditIntentFailed {
        domain_id: i32,
        source_project_id: i32,
        target_project_id: i32,
        reason: String,
    },
    #[error(
        "Database operation '{operation}' failed while enriching custom domain {domain_id} (certificate {certificate_id:?}, environment {environment_id})"
    )]
    EnrichmentDatabase {
        operation: &'static str,
        domain_id: i32,
        certificate_id: Option<i32>,
        environment_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error(
        "Database operation '{operation}' failed while reassigning custom domain {domain_id} from project {source_project_id} to project {target_project_id} environment {target_environment_id}"
    )]
    ReassignmentDatabase {
        operation: &'static str,
        domain_id: i32,
        source_project_id: i32,
        target_project_id: i32,
        target_environment_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },
}

pub struct CustomDomainEnrichment {
    pub custom_domain: project_custom_domains::Model,
    pub certificate: Option<domains::Model>,
    pub environment: Option<environments::Model>,
}

pub struct CustomDomainService {
    db: Arc<DatabaseConnection>,
}

impl CustomDomainService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Rejects anything that is not a syntactically valid hostname.
    ///
    /// The stored value is used as a proxy route key and is rendered as a link
    /// in the console, so a value that is not a hostname at all (a URL, a
    /// scheme, whitespace, a path) has no legitimate use and several harmful
    /// ones. Nothing else on this path checked it: the reserved-hostname guard
    /// only compares against the platform's own names, and the duplicate check
    /// only compares against other rows.
    fn ensure_domain_is_wellformed(domain: &str) -> Result<(), CustomDomainError> {
        let invalid = |reason: &str| {
            Err(CustomDomainError::InvalidDomain(format!(
                "Domain '{domain}' is not a valid hostname: {reason}"
            )))
        };

        if domain.is_empty() {
            return invalid("it is empty");
        }
        if domain.len() > 253 {
            return invalid("it exceeds the 253-character DNS limit");
        }
        if domain != domain.trim() {
            return invalid("it has leading or trailing whitespace");
        }

        // A wildcard is accepted only as a leading `*.` label.
        let host = domain.strip_prefix("*.").unwrap_or(domain);
        if host.contains('*') {
            return invalid("`*` is only allowed as a leading `*.` label");
        }
        if host.starts_with('.') || host.ends_with('.') {
            return invalid("it starts or ends with a dot");
        }

        let labels: Vec<&str> = host.split('.').collect();
        if labels.len() < 2 {
            return invalid("it needs at least two labels (e.g. example.com)");
        }

        for label in labels {
            if label.is_empty() {
                return invalid("it contains an empty label");
            }
            if label.len() > 63 {
                return invalid("a label exceeds the 63-character DNS limit");
            }
            if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return invalid("a label has characters outside a-z, 0-9 and '-'");
            }
            if label.starts_with('-') || label.ends_with('-') {
                return invalid("a label starts or ends with '-'");
            }
        }

        Ok(())
    }

    /// Rejects an `environment_id` that does not belong to `project_id`.
    ///
    /// The caller is authorized against `project_id`, but `environment_id`
    /// arrives in the request body and was written through unchecked. Since
    /// custom domains are read back by `environment_id` alone (with no project
    /// filter), an unchecked value lets a caller with write access to their own
    /// project attach a domain to *another* project's environment.
    async fn ensure_environment_in_project(
        &self,
        environment_id: i32,
        project_id: i32,
    ) -> Result<(), CustomDomainError> {
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                CustomDomainError::InvalidDomain(format!("Environment {environment_id} not found"))
            })?;

        if environment.project_id != project_id {
            // Deliberately does not reveal which project actually owns it.
            return Err(CustomDomainError::InvalidDomain(format!(
                "Environment {environment_id} does not belong to project {project_id}"
            )));
        }

        Ok(())
    }

    /// Rejects domains that the platform itself serves (issue #478).
    ///
    /// Pointing a project at the console hostname (`external_url`) or the
    /// preview-domain apex hijacks the proxy route for the control plane, and
    /// the only way back in is the server's public IP — so the assignment is
    /// refused up front instead of being made and then undone.
    async fn ensure_domain_not_reserved(&self, domain: &str) -> Result<(), CustomDomainError> {
        let settings = settings::Entity::find()
            .one(self.db.as_ref())
            .await?
            .map(|s| AppSettings::from_json(s.data))
            .unwrap_or_default();

        if settings.is_reserved_hostname(domain) {
            return Err(CustomDomainError::InvalidDomain(format!(
                "'{}' is reserved by Temps itself (console or preview domain) and cannot be assigned to a project",
                domain
            )));
        }

        Ok(())
    }

    /// Normalizes a redirect URL by adding https:// scheme if missing
    ///
    /// This handles cases where users provide just a domain name (e.g., "example.com")
    /// instead of a full URL (e.g., "https://example.com").
    fn normalize_redirect_url(redirect_url: &str) -> String {
        let trimmed = redirect_url.trim();

        // If the URL already has a scheme, return as-is.
        // This preserves invalid schemes (ftp://, file://, etc.) so that
        // validate_external_url can reject them.
        if trimmed.contains("://") {
            return trimmed.to_string();
        }

        // Add https:// scheme by default for bare domains like "example.com"
        format!("https://{}", trimmed)
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
    /// * `redirect_url` - The target URL to redirect to (can be domain-only like "example.com")
    /// * `exclude_id` - Optional domain ID to exclude from circular check (for updates)
    ///
    /// # Returns
    /// * `Ok(String)` - The normalized redirect URL if valid
    /// * `Err(CustomDomainError)` if validation fails
    async fn validate_redirect_url(
        &self,
        domain: &str,
        redirect_url: &str,
        exclude_id: Option<i32>,
    ) -> Result<String, CustomDomainError> {
        // Normalize the URL by adding https:// if no scheme is present
        let normalized_url = Self::normalize_redirect_url(redirect_url);

        // Validate URL format and prevent SSRF
        let parsed_url = url_validation::validate_external_url(&normalized_url).map_err(|e| {
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

        Ok(normalized_url)
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
    #[allow(clippy::too_many_arguments)]
    pub async fn create_custom_domain(
        &self,
        project_id: i32,
        environment_id: i32,
        domain: String,
        redirect_to: Option<String>,
        status_code: Option<i32>,
        branch: Option<String>,
        service_name: Option<String>,
    ) -> Result<project_custom_domains::Model, CustomDomainError> {
        info!(
            "Creating custom domain: {} for project: {}",
            domain, project_id
        );

        // Must be a hostname at all before anything else looks at it.
        Self::ensure_domain_is_wellformed(&domain)?;

        // The caller is authorized against `project_id`; `environment_id` comes
        // from the request body and must be proven to belong to it.
        self.ensure_environment_in_project(environment_id, project_id)
            .await?;

        // Refuse hostnames the platform serves itself (console, preview apex)
        self.ensure_domain_not_reserved(&domain).await?;

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

        // Validate and normalize redirect URL if provided
        let normalized_redirect = if let Some(ref redirect_url) = redirect_to {
            if !redirect_url.is_empty() {
                Some(
                    self.validate_redirect_url(&domain, redirect_url, None)
                        .await?,
                )
            } else {
                None
            }
        } else {
            None
        };

        let new_custom_domain = project_custom_domains::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            domain: Set(domain.clone()),
            redirect_to: Set(normalized_redirect),
            status_code: Set(status_code),
            branch: Set(branch),
            status: Set("pending".to_string()),
            message: Set(None),
            certificate_id: Set(None),
            service_name: Set(service_name),
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

    /// Look up a normalized hostname only across projects visible to the caller.
    ///
    /// Applying the caller's hidden-project set in SQL prevents the endpoint
    /// from distinguishing an inaccessible hostname from an unassigned one.
    pub async fn get_visible_custom_domain_by_hostname(
        &self,
        hostname: &str,
        hidden_project_ids: &[i32],
    ) -> Result<Option<project_custom_domains::Model>, CustomDomainError> {
        let normalized_hostname = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
        Self::ensure_domain_is_wellformed(&normalized_hostname)?;

        let mut query = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(normalized_hostname));
        if !hidden_project_ids.is_empty() {
            query = query.filter(
                project_custom_domains::Column::ProjectId
                    .is_not_in(hidden_project_ids.iter().copied()),
            );
        }
        query
            .one(self.db.as_ref())
            .await
            .map_err(CustomDomainError::from)
    }

    /// Load the certificate and environment metadata used by custom-domain responses.
    ///
    /// Keeping these queries in the service preserves the Handler -> Service ->
    /// Data Access boundary. Database sources remain attached for logs while the
    /// public error conversion exposes only operation and resource identifiers.
    pub async fn get_domain_with_info(
        &self,
        custom_domain: project_custom_domains::Model,
    ) -> Result<CustomDomainEnrichment, CustomDomainError> {
        let certificate = if let Some(certificate_id) = custom_domain.certificate_id {
            domains::Entity::find_by_id(certificate_id)
                .one(self.db.as_ref())
                .await
                .map_err(|source| CustomDomainError::EnrichmentDatabase {
                    operation: "load certificate metadata",
                    domain_id: custom_domain.id,
                    certificate_id: custom_domain.certificate_id,
                    environment_id: custom_domain.environment_id,
                    source,
                })?
        } else {
            None
        };

        let environment = environments::Entity::find_by_id(custom_domain.environment_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| CustomDomainError::EnrichmentDatabase {
                operation: "load environment metadata",
                domain_id: custom_domain.id,
                certificate_id: custom_domain.certificate_id,
                environment_id: custom_domain.environment_id,
                source,
            })?;

        Ok(CustomDomainEnrichment {
            custom_domain,
            certificate,
            environment,
        })
    }

    /// Confirm that both sides of a requested reassignment are in the caller's
    /// authorized project scopes before the handler writes its durable intent
    /// audit. The scoped predicates make foreign and absent identifiers
    /// indistinguishable.
    pub async fn validate_reassignment(
        &self,
        id: i32,
        source_project_id: i32,
        target_project_id: i32,
        target_environment_id: i32,
    ) -> Result<(), CustomDomainError> {
        let map_database_error = |operation, source| CustomDomainError::ReassignmentDatabase {
            operation,
            domain_id: id,
            source_project_id,
            target_project_id,
            target_environment_id,
            source,
        };

        let domain_exists = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Id.eq(id))
            .filter(project_custom_domains::Column::ProjectId.eq(source_project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| map_database_error("validate source assignment", source))?
            .is_some();
        if !domain_exists {
            return Err(CustomDomainError::NotFound(format!(
                "Custom domain with ID {id} not found in source project {source_project_id}"
            )));
        }

        let environment_exists = environments::Entity::find()
            .filter(environments::Column::Id.eq(target_environment_id))
            .filter(environments::Column::ProjectId.eq(target_project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(|source| map_database_error("validate target environment", source))?
            .is_some();
        if !environment_exists {
            return Err(CustomDomainError::NotFound(format!(
                "Environment {target_environment_id} not found in target project {target_project_id}"
            )));
        }

        Ok(())
    }

    /// Atomically move a custom domain to another project environment.
    ///
    /// The row and certificate association are preserved, so the proxy keeps
    /// serving the old route until the database trigger publishes the single
    /// committed assignment change and the route table swaps in the new one.
    pub async fn reassign_custom_domain(
        &self,
        id: i32,
        source_project_id: i32,
        target_project_id: i32,
        target_environment_id: i32,
    ) -> Result<project_custom_domains::Model, CustomDomainError> {
        let map_database_error = |operation, source| CustomDomainError::ReassignmentDatabase {
            operation,
            domain_id: id,
            source_project_id,
            target_project_id,
            target_environment_id,
            source,
        };

        let transaction = self
            .db
            .begin()
            .await
            .map_err(|source| map_database_error("begin transaction", source))?;

        let query = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Id.eq(id))
            .filter(project_custom_domains::Column::ProjectId.eq(source_project_id));
        let query = if self.db.get_database_backend() == DatabaseBackend::Postgres {
            query.lock_exclusive()
        } else {
            query
        };
        let custom_domain = query
            .one(&transaction)
            .await
            .map_err(|source| map_database_error("load and lock custom domain", source))?
            .ok_or_else(|| {
                CustomDomainError::NotFound(format!(
                    "Custom domain with ID {id} not found in source project {source_project_id}"
                ))
            })?;

        let target_environment_exists = environments::Entity::find()
            .filter(environments::Column::Id.eq(target_environment_id))
            .filter(environments::Column::ProjectId.eq(target_project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(&transaction)
            .await
            .map_err(|source| map_database_error("load target environment", source))?
            .is_some();
        if !target_environment_exists {
            return Err(CustomDomainError::NotFound(format!(
                "Environment {target_environment_id} not found in target project {target_project_id}"
            )));
        }

        let mut active_model: project_custom_domains::ActiveModel = custom_domain.into();
        active_model.project_id = Set(target_project_id);
        active_model.environment_id = Set(target_environment_id);
        let updated_domain = active_model
            .update(&transaction)
            .await
            .map_err(|source| map_database_error("update domain assignment", source))?;
        transaction
            .commit()
            .await
            .map_err(|source| map_database_error("commit transaction", source))?;

        info!(
            "Reassigned custom domain {} from project {} to project {} environment {}",
            id, source_project_id, target_project_id, target_environment_id
        );
        Ok(updated_domain)
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
        service_name: Option<String>,
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
            // Must be a hostname at all before anything else looks at it.
            Self::ensure_domain_is_wellformed(new_domain)?;

            // Refuse hostnames the platform serves itself (console, preview apex)
            self.ensure_domain_not_reserved(new_domain).await?;

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
            // Re-scope to the row's own project: the caller was authorized
            // against that project, so a moved domain must stay inside it.
            self.ensure_environment_in_project(env_id, custom_domain.project_id)
                .await?;
            active_model.environment_id = Set(env_id);
        }
        if let Some(redirect) = redirect_to {
            // Empty string means clear the field
            if redirect.is_empty() {
                active_model.redirect_to = Set(None);
            } else {
                // Validate and normalize redirect URL before updating
                let normalized_redirect = self
                    .validate_redirect_url(final_domain, &redirect, Some(id))
                    .await?;
                active_model.redirect_to = Set(Some(normalized_redirect));
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
        if let Some(sn) = service_name {
            if sn.is_empty() {
                active_model.service_name = Set(None);
            } else {
                active_model.service_name = Set(Some(sn));
            }
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
    use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
    use temps_entities::{domains, environments, projects, upstream_config::UpstreamList};
    use temps_presets::PresetType;

    async fn test_database() -> Option<temps_database::test_utils::TestDatabase> {
        match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                println!("Docker not available, skipping");
                None
            }
            Err(error) => panic!("failed to create migrated test database: {error}"),
        }
    }

    async fn insert_project_environment(
        db: &Arc<sea_orm::DatabaseConnection>,
        name: &str,
    ) -> (projects::Model, environments::Model) {
        let slug = name.to_ascii_lowercase().replace(' ', "-");
        let project = projects::ActiveModel {
            name: Set(name.to_string()),
            slug: Set(slug.clone()),
            repo_name: Set(format!("{slug}-repo")),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(PresetType::Static),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set(slug.clone()),
            host: Set(format!("{slug}.temps.dev")),
            upstreams: Set(UpstreamList::default()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        (project, environment)
    }

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

    #[test]
    fn wellformed_domains_are_accepted() {
        for domain in [
            "example.com",
            "app.example.com",
            "deep.sub.example.co.uk",
            "*.example.com",
            "my-app-1.example.com",
            "127-0-0-1.sslip.io",
            "xn--80ak6aa92e.com",
        ] {
            assert!(
                CustomDomainService::ensure_domain_is_wellformed(domain).is_ok(),
                "{domain} should be accepted"
            );
        }
    }

    /// The stored value becomes a proxy route key and is rendered as a link in
    /// the console, so anything that is not a hostname must be refused at the
    /// write path rather than relied on to be harmless downstream.
    #[test]
    fn malformed_domains_are_rejected() {
        for domain in [
            "",
            " ",
            " example.com",
            "example.com ",
            "localhost",             // single label
            "example..com",          // empty label
            "-example.com",          // label starts with '-'
            "example-.com",          // label ends with '-'
            ".example.com",          // leading dot
            "example.com.",          // trailing dot
            "http://example.com",    // a URL, not a hostname
            "javascript:alert(1)",   // scheme
            "//evil.com",            // protocol-relative
            "example.com/path",      // path
            "exa mple.com",          // whitespace
            "*.*.example.com",       // wildcard not in leading label
            "ex*ample.com",          // interior wildcard
            "example.com:8080",      // port
            "user:pass@example.com", // credentials
        ] {
            let result = CustomDomainService::ensure_domain_is_wellformed(domain);
            assert!(
                matches!(result, Err(CustomDomainError::InvalidDomain(_))),
                "{domain:?} should be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn domain_length_limits_are_enforced() {
        let long_label = format!("{}.com", "a".repeat(64));
        assert!(CustomDomainService::ensure_domain_is_wellformed(&long_label).is_err());

        let ok_label = format!("{}.com", "a".repeat(63));
        assert!(CustomDomainService::ensure_domain_is_wellformed(&ok_label).is_ok());

        // 253 is the DNS name limit; build something comfortably past it.
        let too_long = format!("{}.com", vec!["a".repeat(50); 6].join("."));
        assert!(too_long.len() > 253);
        assert!(CustomDomainService::ensure_domain_is_wellformed(&too_long).is_err());
    }

    /// The caller is authorized against `project_id`, but `environment_id`
    /// arrives in the request body. Custom domains are read back by
    /// `environment_id` alone, so an unchecked value would let a caller attach
    /// a domain of their choosing to another project's environment.
    #[tokio::test]
    async fn create_rejects_an_environment_from_another_project() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (attacker_project_id, _) = setup_test_data(&test_db.db).await;

        // A second project with its own environment stands in for the victim.
        let victim_project = projects::ActiveModel {
            name: Set("Victim Project".to_string()),
            slug: Set("victim-project".to_string()),
            repo_name: Set("victim-repo".to_string()),
            repo_owner: Set("victim-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(PresetType::Static),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .unwrap();

        let victim_env = environments::ActiveModel {
            project_id: Set(victim_project.id),
            name: Set("production".to_string()),
            slug: Set("production".to_string()),
            subdomain: Set("victim-project-production".to_string()),
            host: Set(String::new()),
            upstreams: Set(UpstreamList::new()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .unwrap();

        let result = service
            .create_custom_domain(
                attacker_project_id,
                victim_env.id,
                "attacker.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await;

        assert!(
            matches!(result, Err(CustomDomainError::InvalidDomain(_))),
            "a cross-project environment_id must be refused, got {result:?}"
        );
    }

    #[tokio::test]
    async fn create_rejects_a_malformed_domain() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let result = service
            .create_custom_domain(
                project_id,
                env_id,
                "javascript:alert(1)".to_string(),
                None,
                None,
                None,
                None,
            )
            .await;

        assert!(
            matches!(result, Err(CustomDomainError::InvalidDomain(_))),
            "a non-hostname must be refused, got {result:?}"
        );
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
                None,
            )
            .await
            .unwrap();

        assert_eq!(domain.domain, "example.com");
        assert_eq!(domain.project_id, project_id);
        assert_eq!(domain.environment_id, env_id);
        assert_eq!(domain.status, "pending");
    }

    /// Issue #478: assigning the console hostname to a project made the
    /// console unreachable — only the raw public IP could get the operator
    /// back in. Create and update must both refuse it.
    #[tokio::test]
    async fn test_console_domain_is_rejected() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        let app_settings = AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            preview_domain: "apps.example.com".to_string(),
            ..Default::default()
        };
        settings::Entity::insert(settings::ActiveModel {
            id: Set(1),
            data: Set(app_settings.to_json()),
            ..sea_orm::ActiveModelBehavior::new()
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(settings::Column::Id)
                .update_column(settings::Column::Data)
                .to_owned(),
        )
        .exec(test_db.db.as_ref())
        .await
        .unwrap();

        for reserved in [
            "console.example.com",
            "CONSOLE.example.com",
            "apps.example.com",
        ] {
            let result = service
                .create_custom_domain(
                    project_id,
                    env_id,
                    reserved.to_string(),
                    None,
                    None,
                    None,
                    None,
                )
                .await;

            match result {
                Err(CustomDomainError::InvalidDomain(msg)) => assert!(
                    msg.contains("reserved"),
                    "expected a reserved-domain message, got: {msg}"
                ),
                other => panic!("expected InvalidDomain for '{reserved}', got: {other:?}"),
            }
        }

        // A normal domain still works, and cannot be renamed onto the console.
        let created = service
            .create_custom_domain(
                project_id,
                env_id,
                "shop.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let result = service
            .update_custom_domain(
                created.id,
                Some("console.example.com".to_string()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(CustomDomainError::InvalidDomain(_))),
            "renaming a domain onto the console hostname must be refused"
        );
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
    async fn test_reassign_custom_domain_valid_target_moves_route_and_preserves_certificate() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (source_project_id, source_environment_id) = setup_test_data(&test_db.db).await;

        let (target_project, target_environment) =
            insert_project_environment(&test_db.db, "Target Project").await;
        let certificate = domains::ActiveModel {
            domain: Set("move.example.com".to_string()),
            status: Set("active".to_string()),
            verification_method: Set("http-01".to_string()),
            is_wildcard: Set(false),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await
        .unwrap();
        let custom_domain = service
            .create_custom_domain(
                source_project_id,
                source_environment_id,
                "move.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        service
            .link_certificate(custom_domain.id, certificate.id)
            .await
            .unwrap();

        let moved = service
            .reassign_custom_domain(
                custom_domain.id,
                source_project_id,
                target_project.id,
                target_environment.id,
            )
            .await
            .unwrap();

        assert_eq!(moved.project_id, target_project.id);
        assert_eq!(moved.environment_id, target_environment.id);
        assert_eq!(moved.certificate_id, Some(certificate.id));
        assert_eq!(moved.domain, "move.example.com");
        assert_eq!(moved.status, "active");
    }

    #[tokio::test]
    async fn test_reassign_custom_domain_missing_domain_returns_not_found() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (source_project_id, _) = setup_test_data(&test_db.db).await;
        let (target_project, target_environment) =
            insert_project_environment(&test_db.db, "Missing Domain Target").await;

        let result = service
            .reassign_custom_domain(
                i32::MAX,
                source_project_id,
                target_project.id,
                target_environment.id,
            )
            .await;

        assert!(matches!(
            result,
            Err(CustomDomainError::NotFound(message))
                if message.contains(&i32::MAX.to_string())
        ));
    }

    #[tokio::test]
    async fn test_validate_reassignment_hides_foreign_domain_and_environment_ownership() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (source_project_id, source_environment_id) = setup_test_data(&test_db.db).await;
        let (foreign_project, foreign_environment) =
            insert_project_environment(&test_db.db, "Foreign Assignment").await;
        let custom_domain = service
            .create_custom_domain(
                source_project_id,
                source_environment_id,
                "scoped-assignment.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let foreign_domain = service
            .validate_reassignment(
                custom_domain.id,
                foreign_project.id,
                foreign_project.id,
                foreign_environment.id,
            )
            .await;
        let missing_domain = service
            .validate_reassignment(
                i32::MAX,
                foreign_project.id,
                foreign_project.id,
                foreign_environment.id,
            )
            .await;
        assert!(matches!(
            foreign_domain,
            Err(CustomDomainError::NotFound(_))
        ));
        assert!(matches!(
            missing_domain,
            Err(CustomDomainError::NotFound(_))
        ));

        let foreign_environment_result = service
            .validate_reassignment(
                custom_domain.id,
                source_project_id,
                source_project_id,
                foreign_environment.id,
            )
            .await;
        let missing_environment_result = service
            .validate_reassignment(
                custom_domain.id,
                source_project_id,
                source_project_id,
                i32::MAX,
            )
            .await;
        assert!(matches!(
            foreign_environment_result,
            Err(CustomDomainError::NotFound(_))
        ));
        assert!(matches!(
            missing_environment_result,
            Err(CustomDomainError::NotFound(_))
        ));

        let persisted = service
            .get_custom_domain(custom_domain.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.project_id, source_project_id);
        assert_eq!(persisted.environment_id, source_environment_id);
    }

    #[tokio::test]
    async fn test_reassign_custom_domain_missing_or_deleted_target_returns_not_found_without_mutation(
    ) {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (source_project_id, source_environment_id) = setup_test_data(&test_db.db).await;
        let (target_project, target_environment) =
            insert_project_environment(&test_db.db, "Deleted Target").await;
        let custom_domain = service
            .create_custom_domain(
                source_project_id,
                source_environment_id,
                "rollback-target.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        for target_environment_id in [i32::MAX, target_environment.id] {
            if target_environment_id == target_environment.id {
                let mut deleted_environment = target_environment.clone().into_active_model();
                deleted_environment.deleted_at = Set(Some(chrono::Utc::now()));
                deleted_environment
                    .update(test_db.db.as_ref())
                    .await
                    .unwrap();
            }

            let result = service
                .reassign_custom_domain(
                    custom_domain.id,
                    source_project_id,
                    target_project.id,
                    target_environment_id,
                )
                .await;

            assert!(matches!(
                result,
                Err(CustomDomainError::NotFound(message))
                    if message.contains(&target_environment_id.to_string())
            ));
            let persisted = service
                .get_custom_domain(custom_domain.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(persisted.project_id, source_project_id);
            assert_eq!(persisted.environment_id, source_environment_id);
        }
    }

    #[tokio::test]
    async fn test_reassign_custom_domain_wrong_target_project_returns_not_found_without_mutation() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (source_project_id, source_environment_id) = setup_test_data(&test_db.db).await;
        let (actual_target_project, target_environment) =
            insert_project_environment(&test_db.db, "Actual Target").await;
        let (wrong_target_project, _) =
            insert_project_environment(&test_db.db, "Wrong Target").await;
        let custom_domain = service
            .create_custom_domain(
                source_project_id,
                source_environment_id,
                "wrong-project.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let result = service
            .reassign_custom_domain(
                custom_domain.id,
                source_project_id,
                wrong_target_project.id,
                target_environment.id,
            )
            .await;

        assert!(matches!(
            result,
            Err(CustomDomainError::NotFound(message))
                if message.contains(&target_environment.id.to_string())
                    && message.contains(&wrong_target_project.id.to_string())
        ));
        assert_ne!(actual_target_project.id, wrong_target_project.id);
        let persisted = service
            .get_custom_domain(custom_domain.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.project_id, source_project_id);
        assert_eq!(persisted.environment_id, source_environment_id);
    }

    #[tokio::test]
    async fn test_reassign_custom_domain_stale_source_returns_not_found_without_mutation() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (source_project_id, source_environment_id) = setup_test_data(&test_db.db).await;
        let (target_project, target_environment) =
            insert_project_environment(&test_db.db, "Stale Target").await;
        let custom_domain = service
            .create_custom_domain(
                source_project_id,
                source_environment_id,
                "stale.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        service
            .reassign_custom_domain(
                custom_domain.id,
                source_project_id,
                target_project.id,
                target_environment.id,
            )
            .await
            .unwrap();

        let stale_retry = service
            .reassign_custom_domain(
                custom_domain.id,
                source_project_id,
                target_project.id,
                target_environment.id,
            )
            .await;
        assert!(matches!(
            stale_retry,
            Err(CustomDomainError::NotFound(message))
                if message.contains(&custom_domain.id.to_string())
                    && message.contains(&source_project_id.to_string())
        ));
        let persisted = service
            .get_custom_domain(custom_domain.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.project_id, target_project.id);
        assert_eq!(persisted.environment_id, target_environment.id);
    }

    #[tokio::test]
    async fn test_get_visible_custom_domain_by_hostname_inaccessible_and_missing_are_indistinguishable(
    ) {
        let Some(test_db) = test_database().await else {
            return;
        };
        let service = CustomDomainService::new(test_db.db.clone());
        let (owner_project_id, owner_environment_id) = setup_test_data(&test_db.db).await;
        let custom_domain = service
            .create_custom_domain(
                owner_project_id,
                owner_environment_id,
                "private.example.com".to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let inaccessible = service
            .get_visible_custom_domain_by_hostname("private.example.com", &[owner_project_id])
            .await
            .unwrap();
        let missing = service
            .get_visible_custom_domain_by_hostname("missing.example.com", &[owner_project_id])
            .await
            .unwrap();

        assert_eq!(inaccessible, missing);
        assert!(inaccessible.is_none());

        let accessible = service
            .get_visible_custom_domain_by_hostname("PRIVATE.EXAMPLE.COM.", &[])
            .await
            .unwrap();
        assert_eq!(accessible.map(|domain| domain.id), Some(custom_domain.id));
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
    async fn test_create_domain_with_redirect_without_scheme_normalizes_to_https() {
        let test_db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let service = CustomDomainService::new(test_db.db.clone());
        let (project_id, env_id) = setup_test_data(&test_db.db).await;

        // Redirect URL without scheme should be normalized to https://
        let domain = service
            .create_custom_domain(
                project_id,
                env_id,
                "www.example.com".to_string(),
                Some("example.com".to_string()), // No https:// prefix
                Some(301),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(domain.domain, "www.example.com");
        // Should be normalized to include https:// scheme
        assert_eq!(domain.redirect_to, Some("https://example.com".to_string()));
        assert_eq!(domain.status_code, Some(301));
    }

    #[tokio::test]
    async fn test_normalize_redirect_url() {
        // Test without scheme - should add https://
        assert_eq!(
            CustomDomainService::normalize_redirect_url("example.com"),
            "https://example.com"
        );

        // Test with https:// - should remain unchanged
        assert_eq!(
            CustomDomainService::normalize_redirect_url("https://example.com"),
            "https://example.com"
        );

        // Test with http:// - should remain unchanged
        assert_eq!(
            CustomDomainService::normalize_redirect_url("http://example.com"),
            "http://example.com"
        );

        // Test with whitespace - should trim and add https://
        assert_eq!(
            CustomDomainService::normalize_redirect_url("  example.com  "),
            "https://example.com"
        );

        // Test with path - should add https://
        assert_eq!(
            CustomDomainService::normalize_redirect_url("example.com/path"),
            "https://example.com/path"
        );
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
                .create_custom_domain(project_id, env_id, domain, redirect, Some(301), None, None)
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
