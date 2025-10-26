use chrono::Utc;
use rand::Rng;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use temps_entities::{project_dsns, projects};

use super::types::{ParsedDSN, ProjectDSN, SentryIngesterError};

/// Service for managing Data Source Names (DSNs) for error tracking
pub struct DSNService {
    db: Arc<DatabaseConnection>,
}

impl DSNService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Generate a new DSN for a project
    pub async fn generate_project_dsn(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        name: Option<String>,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::ProjectNotFound)?;

        // Generate secure public key only (secret key is deprecated)
        let public_key = self.generate_key(32);
        let secret_key = String::new(); // Deprecated - kept empty for compatibility

        // Create new DSN record
        let new_dsn = project_dsns::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            deployment_id: Set(deployment_id),
            name: Set(name.unwrap_or_else(|| "Default DSN".to_string())),
            public_key: Set(public_key.clone()),
            secret_key: Set(secret_key.clone()),
            is_active: Set(true),
            rate_limit_per_minute: Set(Some(1000)),
            allowed_origins: Set(None),
            last_used_at: Set(None),
            event_count: Set(0),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let dsn_model = new_dsn.insert(self.db.as_ref()).await?;

        // Build DSN in Sentry-compatible format
        // Format: https://PUBLIC_KEY@HOST/PROJECT_ID
        let host = base_url
            .replace("https://", "")
            .replace("http://", "")
            .replace(":8080", ""); // Remove common dev port

        let dsn = format!("https://{}@{}/{}", dsn_model.public_key, host, project_id);

        Ok(ProjectDSN {
            id: dsn_model.id,
            project_id,
            environment_id: dsn_model.environment_id,
            deployment_id: dsn_model.deployment_id,
            name: dsn_model.name,
            public_key: dsn_model.public_key,
            secret_key: dsn_model.secret_key,
            dsn,
            created_at: dsn_model.created_at,
            is_active: dsn_model.is_active,
            event_count: dsn_model.event_count,
        })
    }

    /// Get or create DSN for a project/environment/deployment
    pub async fn get_or_create_project_dsn(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        // Check if DSN already exists
        let mut query = project_dsns::Entity::find()
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .filter(project_dsns::Column::IsActive.eq(true));

        if let Some(env_id) = environment_id {
            query = query.filter(project_dsns::Column::EnvironmentId.eq(env_id));
        } else {
            query = query.filter(project_dsns::Column::EnvironmentId.is_null());
        }

        if let Some(deploy_id) = deployment_id {
            query = query.filter(project_dsns::Column::DeploymentId.eq(deploy_id));
        } else {
            query = query.filter(project_dsns::Column::DeploymentId.is_null());
        }

        if let Some(existing_dsn) = query.one(self.db.as_ref()).await? {
            // Return existing DSN
            let (protocol, host_with_port) = if base_url.starts_with("https://") {
                ("https", base_url.strip_prefix("https://").unwrap())
            } else if base_url.starts_with("http://") {
                ("http", base_url.strip_prefix("http://").unwrap())
            } else {
                ("https", base_url)
            };

            let dsn = format!(
                "{}://{}@{}/{}",
                protocol, existing_dsn.public_key, host_with_port, project_id
            );

            return Ok(ProjectDSN {
                id: existing_dsn.id,
                project_id,
                environment_id: existing_dsn.environment_id,
                deployment_id: existing_dsn.deployment_id,
                name: existing_dsn.name,
                public_key: existing_dsn.public_key,
                secret_key: existing_dsn.secret_key,
                dsn,
                created_at: existing_dsn.created_at,
                is_active: existing_dsn.is_active,
                event_count: existing_dsn.event_count,
            });
        }

        // Create new DSN if none exists
        let name = match (environment_id, deployment_id) {
            (Some(_), Some(_)) => "Environment-Deployment DSN".to_string(),
            (Some(_), None) => "Environment DSN".to_string(),
            (None, Some(_)) => "Deployment DSN".to_string(),
            (None, None) => "Project DSN".to_string(),
        };

        self.generate_project_dsn(
            project_id,
            environment_id,
            deployment_id,
            Some(name),
            base_url,
        )
        .await
    }

    /// Create a new DSN without checking for duplicates
    pub async fn create_project_dsn(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        name: Option<String>,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        let name = name.unwrap_or_else(|| match (environment_id, deployment_id) {
            (Some(_), Some(_)) => "Environment-Deployment DSN".to_string(),
            (Some(_), None) => "Environment DSN".to_string(),
            (None, Some(_)) => "Deployment DSN".to_string(),
            (None, None) => "Project DSN".to_string(),
        });

        self.generate_project_dsn(
            project_id,
            environment_id,
            deployment_id,
            Some(name),
            base_url,
        )
        .await
    }

    /// Parse a DSN string
    pub fn parse_dsn(&self, dsn: &str) -> Result<ParsedDSN, SentryIngesterError> {
        let url = url::Url::parse(dsn).map_err(|_| SentryIngesterError::InvalidDSN)?;

        let protocol = url.scheme().to_string();
        let host = url
            .host_str()
            .ok_or(SentryIngesterError::InvalidDSN)?
            .to_string();

        let public_key = url.username().to_string();
        if public_key.is_empty() {
            return Err(SentryIngesterError::InvalidDSN);
        }

        let project_id = url
            .path()
            .trim_start_matches('/')
            .parse::<i32>()
            .map_err(|_| SentryIngesterError::InvalidDSN)?;

        tracing::debug!(
            "Parsed DSN - public_key: {}, project_id: {}, from path: {}",
            public_key,
            project_id,
            url.path()
        );

        Ok(ParsedDSN {
            public_key,
            project_id,
            host,
            protocol,
        })
    }

    /// Validate DSN authentication
    pub async fn validate_dsn_auth(
        &self,
        parsed_dsn: &ParsedDSN,
    ) -> Result<(bool, Option<project_dsns::Model>), SentryIngesterError> {
        tracing::debug!(
            "Validating DSN auth for project {} with public key {}",
            parsed_dsn.project_id,
            parsed_dsn.public_key
        );

        let dsn = project_dsns::Entity::find()
            .filter(project_dsns::Column::ProjectId.eq(parsed_dsn.project_id))
            .filter(project_dsns::Column::PublicKey.eq(&parsed_dsn.public_key))
            .filter(project_dsns::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?;

        tracing::debug!("DSN lookup result: {:?}", dsn.is_some());

        match dsn {
            Some(dsn_record) => Ok((true, Some(dsn_record))),
            None => Ok((false, None)),
        }
    }

    /// Validate DSN and return ProjectDSN if valid
    pub async fn validate_dsn(
        &self,
        project_id: i32,
        public_key: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        let dsn_record = project_dsns::Entity::find()
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .filter(project_dsns::Column::PublicKey.eq(public_key))
            .filter(project_dsns::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::InvalidDSN)?;

        let dsn_string = format!(
            "https://{}@sentry.io/{}",
            dsn_record.public_key, dsn_record.project_id
        );

        Ok(ProjectDSN {
            id: dsn_record.id,
            project_id: dsn_record.project_id,
            environment_id: dsn_record.environment_id,
            deployment_id: dsn_record.deployment_id,
            name: dsn_record.name,
            public_key: dsn_record.public_key,
            secret_key: dsn_record.secret_key,
            dsn: dsn_string,
            created_at: dsn_record.created_at,
            is_active: dsn_record.is_active,
            event_count: dsn_record.event_count,
        })
    }

    /// Get project by public key
    pub async fn get_project_by_public_key(
        &self,
        public_key: &str,
    ) -> Result<Option<project_dsns::Model>, SentryIngesterError> {
        let dsn = project_dsns::Entity::find()
            .filter(project_dsns::Column::PublicKey.eq(public_key))
            .filter(project_dsns::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?;

        Ok(dsn)
    }

    /// Regenerate DSN (rotate keys)
    pub async fn regenerate_project_dsn(
        &self,
        dsn_id: i32,
        project_id: i32,
        base_url: &str,
    ) -> Result<ProjectDSN, SentryIngesterError> {
        // Get existing DSN
        let existing_dsn = project_dsns::Entity::find_by_id(dsn_id)
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::InvalidDSN)?;

        // Generate new keys
        let new_public_key = self.generate_key(32);
        let new_secret_key = String::new(); // Deprecated

        // Update DSN
        let mut dsn_update: project_dsns::ActiveModel = existing_dsn.into();
        dsn_update.public_key = Set(new_public_key.clone());
        dsn_update.secret_key = Set(new_secret_key.clone());
        dsn_update.updated_at = Set(Utc::now());

        let updated_dsn = dsn_update.update(self.db.as_ref()).await?;

        // Build new DSN string
        let host = base_url
            .replace("https://", "")
            .replace("http://", "")
            .replace(":8080", "");

        let dsn = format!("https://{}@{}/{}", updated_dsn.public_key, host, project_id);

        Ok(ProjectDSN {
            id: updated_dsn.id,
            project_id,
            environment_id: updated_dsn.environment_id,
            deployment_id: updated_dsn.deployment_id,
            name: updated_dsn.name,
            public_key: updated_dsn.public_key,
            secret_key: updated_dsn.secret_key,
            dsn,
            created_at: updated_dsn.created_at,
            is_active: updated_dsn.is_active,
            event_count: updated_dsn.event_count,
        })
    }

    /// List all DSNs for a project
    pub async fn list_project_dsns(
        &self,
        project_id: i32,
        base_url: &str,
    ) -> Result<Vec<ProjectDSN>, SentryIngesterError> {
        let dsns = project_dsns::Entity::find()
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .all(self.db.as_ref())
            .await?;

        // Parse base URL to get host
        let (protocol, host_with_port) = if base_url.starts_with("https://") {
            ("https", base_url.strip_prefix("https://").unwrap())
        } else if base_url.starts_with("http://") {
            ("http", base_url.strip_prefix("http://").unwrap())
        } else {
            ("https", base_url)
        };

        Ok(dsns
            .into_iter()
            .map(|dsn| ProjectDSN {
                id: dsn.id,
                project_id: dsn.project_id,
                environment_id: dsn.environment_id,
                deployment_id: dsn.deployment_id,
                name: dsn.name.clone(),
                public_key: dsn.public_key.clone(),
                secret_key: dsn.secret_key.clone(),
                dsn: format!(
                    "{}://{}@{}/{}",
                    protocol, dsn.public_key, host_with_port, dsn.project_id
                ),
                created_at: dsn.created_at,
                is_active: dsn.is_active,
                event_count: dsn.event_count,
            })
            .collect())
    }

    /// Revoke (deactivate) a DSN
    pub async fn revoke_dsn(
        &self,
        dsn_id: i32,
        project_id: i32,
    ) -> Result<(), SentryIngesterError> {
        let dsn = project_dsns::Entity::find_by_id(dsn_id)
            .filter(project_dsns::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(SentryIngesterError::InvalidDSN)?;

        let mut dsn_update: project_dsns::ActiveModel = dsn.into();
        dsn_update.is_active = Set(false);
        dsn_update.updated_at = Set(Utc::now());
        dsn_update.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Generate a random key
    fn generate_key(&self, length: usize) -> String {
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..length).map(|_| rng.gen()).collect();
        hex::encode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{preset::Preset, projects};

    async fn setup_test_db() -> TestDatabase {
        TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database")
    }

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
        use uuid::Uuid;

        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        project
            .insert(db.as_ref())
            .await
            .expect("Failed to create project")
            .id
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_generate_project_dsn() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());

        let project_id = create_test_project(&db).await;

        let dsn = service
            .generate_project_dsn(
                project_id,
                None,
                None,
                Some("Test DSN".to_string()),
                "https://example.com",
            )
            .await
            .expect("Failed to generate DSN");

        assert_eq!(dsn.project_id, project_id);
        assert_eq!(dsn.name, "Test DSN");
        assert!(!dsn.public_key.is_empty());
        assert!(dsn.dsn.contains(&dsn.public_key));
        assert!(dsn.is_active);
    }

    #[tokio::test]
    async fn test_parse_dsn() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db);

        let dsn_str = "https://abc123@example.com/42";
        let parsed = service.parse_dsn(dsn_str).expect("Failed to parse DSN");

        assert_eq!(parsed.public_key, "abc123");
        assert_eq!(parsed.project_id, 42);
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.protocol, "https");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_validate_dsn_auth() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());

        let project_id = create_test_project(&db).await;

        // Generate a DSN
        let dsn = service
            .generate_project_dsn(project_id, None, None, None, "https://example.com")
            .await
            .expect("Failed to generate DSN");

        // Parse it
        let parsed = service.parse_dsn(&dsn.dsn).expect("Failed to parse DSN");

        // Validate it
        let (is_valid, record) = service
            .validate_dsn_auth(&parsed)
            .await
            .expect("Failed to validate DSN");

        assert!(is_valid);
        assert!(record.is_some());
        assert_eq!(record.unwrap().project_id, project_id);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_revoke_dsn() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = DSNService::new(db.clone());

        let project_id = create_test_project(&db).await;

        let dsn = service
            .generate_project_dsn(project_id, None, None, None, "https://example.com")
            .await
            .expect("Failed to generate DSN");

        // Revoke it
        service
            .revoke_dsn(dsn.id, project_id)
            .await
            .expect("Failed to revoke DSN");

        // Try to validate - should fail
        let parsed = service.parse_dsn(&dsn.dsn).expect("Failed to parse DSN");
        let (is_valid, _) = service
            .validate_dsn_auth(&parsed)
            .await
            .expect("Failed to validate DSN");

        assert!(!is_valid);
    }
}
