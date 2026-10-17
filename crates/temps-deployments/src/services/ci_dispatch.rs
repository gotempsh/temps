// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database-backed GitHub Actions dispatch. All work is on the control plane;
//! active rows bound reconciliation and one-build tokens never authorize normal APIs.
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc, time::Duration};
use temps_core::EncryptionService;
use temps_deployer::{
    build_protocol::{BuildLimits, BuildSpec},
    build_verification::{
        verify_runner_report, DispatchedBuild, ImageDigest, ObservedImage, RunnerReport,
    },
    runner_protocol::{RunnerResult, RunnerWork},
    BuilderError,
};
use uuid::Uuid;

#[derive(thiserror::Error, Debug)]
pub enum CiError {
    #[error("CI builder configuration: {0}")]
    Configuration(String),
    #[error("CI build {build_id}: provider run is not yet recorded")]
    NotReady { build_id: String },
    #[error("CI build {build_id}: {reason}")]
    Build { build_id: String, reason: String },
    #[error("CI runner result rejected for build {build_id}: {reason}")]
    Verification { build_id: String, reason: String },
    #[error("CI runner identity rejected for build {build_id}: {reason}")]
    Identity { build_id: String, reason: String },
    #[error("CI dispatch database operation failed: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("CI dispatch storage operation failed: {0}")]
    Io(#[from] std::io::Error),
}
impl From<CiError> for BuilderError {
    fn from(error: CiError) -> Self {
        Self::Other(error.to_string())
    }
}

/// Public trust policy; no hardcoded provider-specific token validator.
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TrustedIdentityIssuer {
    pub issuer: String,
    pub jwks_url: String,
    pub audience: String,
    pub subject: String,
}
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CiExecutorSettings {
    pub owner: String,
    pub repository: String,
    pub organization: bool,
    pub git_ref: String,
    pub control_plane_url: String,
    /// Pinned release archive used by the generated CI shim.
    pub runner_archive_url: String,
    pub runner_archive_sha256: String,
    pub identity: TrustedIdentityIssuer,
    pub image_repository: String,
    pub limits: BuildLimits,
    /// Write-only. Stored encrypted; omitted by the status endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(write_only)]
    pub dispatch_token: Option<String>,
    /// Repository-scoped push/pull credentials. Never included in status.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(write_only)]
    pub registry_credentials: Option<temps_deployer::remote::RemotePullCredentials>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct CiExecutorStatus {
    pub configured: bool,
    pub reason: Option<String>,
    pub setup_path: String,
    pub settings: Option<CiExecutorSettings>,
}
#[derive(Clone, Serialize, Deserialize)]
struct AutomaticSubject {
    subject: String,
    owner: String,
    repository: String,
    git_ref: String,
}
#[derive(Serialize, Deserialize)]
struct StoredExecutor {
    #[serde(flatten)]
    settings: CiExecutorSettings,
    #[serde(default)]
    automatic_subject: Option<AutomaticSubject>,
}
#[derive(FromQueryResult)]
struct StoredConfig {
    config_encrypted: String,
}
#[derive(FromQueryResult)]
pub struct CiBuild {
    pub id: Uuid,
    pub deployment_id: Option<i32>,
    pub state: String,
    pub run_id: Option<String>,
    pub token_id: Option<i32>,
    pub spec_encrypted: String,
    pub executor_encrypted: String,
    pub image_repository: String,
    pub result: Option<serde_json::Value>,
    pub failure: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}
fn statement(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}
fn build_error(id: Uuid, reason: impl Into<String>) -> CiError {
    CiError::Build {
        build_id: id.to_string(),
        reason: reason.into(),
    }
}
struct ContextGuard(PathBuf);
impl Drop for ContextGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "Cannot remove CI context archive");
            }
        }
    }
}

#[derive(Clone)]
pub struct CiDispatchService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    root: PathBuf,
    client: reqwest::Client,
    #[cfg(test)]
    test_origin: Option<String>,
}
impl CiExecutorSettings {
    pub fn validate(&self) -> Result<(), CiError> {
        self.limits.validate().map_err(CiError::Configuration)?;
        for value in [&self.owner, &self.repository] {
            if value.is_empty()
                || value.len() > 100
                || !value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
                || value == "."
                || value == ".."
            {
                return Err(CiError::Configuration(
                    "Owner and builder repository must be plain repository names".into(),
                ));
            }
        }
        for value in [
            &self.control_plane_url,
            &self.runner_archive_url,
            &self.identity.issuer,
            &self.identity.jwks_url,
        ] {
            if value.len() > 4096 {
                return Err(CiError::Configuration("CI URL exceeds 4096 bytes".into()));
            }
            let url = reqwest::Url::parse(value)
                .map_err(|_| CiError::Configuration("Expected an absolute HTTPS URL".into()))?;
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || value
                    .chars()
                    .any(|c| matches!(c, '\'' | '"' | '`' | '$' | '\\' | '\n' | '\r'))
            {
                return Err(CiError::Configuration("CI URLs require HTTPS without URL credentials, query strings, fragments or shell metacharacters".into()));
            }
        }
        if self.git_ref.is_empty()
            || self.git_ref.len() > 128
            || !self
                .git_ref
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/'))
        {
            return Err(CiError::Configuration(
                "Invalid builder repository ref".into(),
            ));
        }
        ImageDigest::parse(&format!("sha256:{}", self.runner_archive_sha256))
            .map_err(|e| CiError::Configuration(e.to_string()))?;
        temps_deployer::build_verification::validate_repository(0, &self.image_repository)
            .map_err(|error| CiError::Configuration(error.to_string()))?;
        if !self.image_repository.contains('/') {
            return Err(CiError::Configuration(
                "Registry repository must include an explicit registry host and path".into(),
            ));
        }
        if self.identity.audience.len() > 256
            || self.identity.subject.len() > 512
            || self.identity.audience.is_empty()
        {
            return Err(CiError::Configuration(
                "CI identity requires an audience of at most 256 bytes and a subject of at most 512 bytes".into(),
            ));
        }
        Ok(())
    }
}
impl CiDispatchService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        data_dir: PathBuf,
    ) -> Result<Self, CiError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("temps-ci-dispatch")
            .build()
            .map_err(|e| CiError::Configuration(e.to_string()))?;
        Ok(Self {
            db,
            encryption,
            root: data_dir.join("ci-builds"),
            client,
            #[cfg(test)]
            test_origin: None,
        })
    }
    fn endpoint(&self, url: String) -> String {
        #[cfg(test)]
        if let Some(origin) = &self.test_origin {
            if let Ok(parsed) = reqwest::Url::parse(&url) {
                return format!("{origin}{}", parsed.path());
            }
        }
        url
    }
    async fn config(&self) -> Result<Option<CiExecutorSettings>, CiError> {
        self.config_on(self.db.as_ref()).await
    }
    async fn config_on<C: ConnectionTrait>(
        &self,
        connection: &C,
    ) -> Result<Option<CiExecutorSettings>, CiError> {
        Ok(self
            .stored_config_on(connection)
            .await?
            .map(|stored| stored.settings))
    }
    async fn stored_config_on<C: ConnectionTrait>(
        &self,
        connection: &C,
    ) -> Result<Option<StoredExecutor>, CiError> {
        let row = StoredConfig::find_by_statement(statement(
            "SELECT config_encrypted FROM ci_build_executor WHERE id = 1",
            vec![],
        ))
        .one(connection)
        .await?;
        row.map(|row| {
            let bytes = self
                .encryption
                .decrypt(&row.config_encrypted)
                .map_err(|_| {
                    CiError::Configuration("Cannot decrypt CI executor configuration".into())
                })?;
            serde_json::from_slice(&bytes).map_err(|_| {
                CiError::Configuration("Stored CI executor configuration is invalid".into())
            })
        })
        .transpose()
    }
    pub async fn status(&self) -> Result<CiExecutorStatus, CiError> {
        let settings = self.config().await?.map(|mut config| {
            config.dispatch_token = None;
            config.registry_credentials = None;
            config
        });
        let configured = settings
            .as_ref()
            .is_some_and(|settings| !settings.identity.subject.is_empty());
        let reason = if settings.is_none() {
            Some("No CI builder is configured. Configure a builder repository to run source builds in GitHub Actions.".into())
        } else if !configured {
            Some("The builder repository identity is not configured. Provision the repository to record its immutable identity, or set an exact OIDC subject.".into())
        } else {
            None
        };
        Ok(CiExecutorStatus {
            configured,
            reason,
            setup_path: "/settings/nodes#ci-builder".into(),
            settings,
        })
    }
    pub async fn save(&self, config: CiExecutorSettings) -> Result<(), CiError> {
        self.save_if_unchanged(config, None, None).await
    }
    async fn save_if_unchanged(
        &self,
        mut config: CiExecutorSettings,
        expected: Option<Vec<u8>>,
        provisioned_subject: Option<AutomaticSubject>,
    ) -> Result<(), CiError> {
        config.validate()?;
        let transaction = self.db.begin().await?;
        transaction
            .execute(statement("SELECT pg_advisory_xact_lock(1528, 1)", vec![]))
            .await?;
        let mut automatic_subject = provisioned_subject;
        if let Some(stored) = self.stored_config_on(&transaction).await? {
            let previous = stored.settings;
            if expected.is_none() {
                if let Some(automatic) = stored.automatic_subject {
                    // Unchanged generated text remains managed by Temps. An
                    // explicitly edited subject becomes operator-owned instead.
                    if config.identity.subject == automatic.subject {
                        if config.owner != automatic.owner
                            || config.repository != automatic.repository
                            || config.git_ref != automatic.git_ref
                        {
                            config.identity.subject.clear();
                        } else {
                            automatic_subject = Some(automatic);
                        }
                    }
                }
            }
            if let Some(expected) = &expected {
                let actual = serde_json::to_vec(&previous)
                    .map_err(|error| CiError::Configuration(error.to_string()))?;
                if actual != *expected {
                    return Err(CiError::Configuration("CI settings changed while provisioning. Refresh the settings and provision again".into()));
                }
            }
            config.dispatch_token = config.dispatch_token.or(previous.dispatch_token);
            config.registry_credentials = config
                .registry_credentials
                .or(previous.registry_credentials);
        } else if expected.is_some() {
            return Err(CiError::Configuration(
                "CI settings were removed while provisioning. Refresh and provision again".into(),
            ));
        }
        if config.dispatch_token.as_deref().is_none_or(str::is_empty) {
            return Err(CiError::Configuration(
                "A repository API token is required".into(),
            ));
        }
        let bytes = serde_json::to_vec(&StoredExecutor {
            settings: config,
            automatic_subject,
        })
        .map_err(|e| CiError::Configuration(e.to_string()))?;
        let encrypted = self.encryption.encrypt(&bytes).map_err(|_| {
            CiError::Configuration("Could not encrypt CI executor configuration".into())
        })?;
        transaction.execute(statement("INSERT INTO ci_build_executor (id, config_encrypted) VALUES (1, $1) ON CONFLICT (id) DO UPDATE SET config_encrypted = EXCLUDED.config_encrypted", vec![encrypted.into()])).await?;
        transaction.commit().await?;
        Ok(())
    }
    fn github(
        &self,
        config: &CiExecutorSettings,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(
                method,
                self.endpoint(format!("https://api.github.com{path}")),
            )
            .bearer_auth(config.dispatch_token.as_deref().unwrap_or_default())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }
    fn provider_error(
        config: &CiExecutorSettings,
        operation: &str,
        reason: impl std::fmt::Display,
    ) -> CiError {
        CiError::Configuration(format!(
            "CI provider {operation} for builder repository {}/{}: {reason}",
            config.owner, config.repository
        ))
    }
    async fn provider_request(
        &self,
        config: &CiExecutorSettings,
        operation: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CiError> {
        request.send().await.map_err(|error| {
            // Request URLs can contain credentials/query parameters. Preserve
            // the transport cause, never a URL or provider response body.
            let error = error.without_url();
            let mut reason = error.to_string();
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                reason.push_str(": ");
                reason.push_str(&cause.to_string());
                source = cause.source();
            }
            Self::provider_error(config, operation, reason)
        })
    }
    async fn checked(
        &self,
        config: &CiExecutorSettings,
        operation: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CiError> {
        let response = self.provider_request(config, operation, request).await?;
        if !response.status().is_success() {
            return Err(Self::provider_error(
                config,
                operation,
                format!("HTTP {}", response.status()),
            ));
        }
        Ok(response)
    }
    pub async fn provision(&self) -> Result<(), CiError> {
        use base64::Engine;
        let stored = self
            .stored_config_on(self.db.as_ref())
            .await?
            .ok_or_else(|| CiError::Configuration("Save CI builder settings first".into()))?;
        let mut config = stored.settings;
        let mut automatic_subject = stored.automatic_subject;
        let original_config = serde_json::to_vec(&config)
            .map_err(|error| CiError::Configuration(error.to_string()))?;
        let repo = format!("/repos/{}/{}", config.owner, config.repository);
        let response = self
            .provider_request(
                &config,
                "inspect builder repository",
                self.github(&config, reqwest::Method::GET, &repo),
            )
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            if !config.organization {
                #[derive(Deserialize)]
                struct Account {
                    login: String,
                }
                let response = self
                    .checked(
                        &config,
                        "inspect API token account",
                        self.github(&config, reqwest::Method::GET, "/user"),
                    )
                    .await?;
                let account: Account = self.read_json(response, 64 * 1024).await?;
                if !account.login.eq_ignore_ascii_case(&config.owner) {
                    return Err(CiError::Configuration("Personal builder repository owner must match the API token's account; select Organization for an organization repository".into()));
                }
                config.owner = account.login;
            }
            let path = if config.organization {
                format!("/orgs/{}/repos", config.owner)
            } else {
                "/user/repos".into()
            };
            self.checked(&config, "create private repository", self.github(&config, reqwest::Method::POST, &path).json(
                &serde_json::json!({"name": config.repository, "private": true, "auto_init": true}),
            ))
            .await?;
        } else if !response.status().is_success() {
            return Err(Self::provider_error(
                &config,
                "inspect builder repository",
                format!("HTTP {}", response.status()),
            ));
        }
        if config.identity.subject.is_empty() {
            #[derive(Deserialize)]
            struct Repository {
                id: u64,
                full_name: String,
            }
            let response = self
                .checked(
                    &config,
                    "read immutable repository identity",
                    self.github(&config, reqwest::Method::GET, &repo),
                )
                .await?;
            let repository: Repository = self.read_json(response, 128 * 1024).await?;
            if !repository
                .full_name
                .eq_ignore_ascii_case(&format!("{}/{}", config.owner, config.repository))
            {
                return Err(CiError::Configuration(
                    "Provider returned another builder repository identity".into(),
                ));
            }
            if let Some((owner, name)) = repository.full_name.split_once('/') {
                config.owner = owner.into();
                config.repository = name.into();
            }
            self.checked(&config, "configure OIDC subject", self.github(&config, reqwest::Method::PUT, &format!("{repo}/actions/oidc/customization/sub"))
                .json(&serde_json::json!({"use_default": false, "include_claim_keys": ["repository_id", "ref"]}))).await?;
            let git_ref = if config.git_ref.starts_with("refs/") {
                config.git_ref.clone()
            } else {
                format!("refs/heads/{}", config.git_ref)
            };
            config.identity.subject = format!("repository_id:{}:ref:{git_ref}", repository.id);
            automatic_subject = Some(AutomaticSubject {
                subject: config.identity.subject.clone(),
                owner: config.owner.clone(),
                repository: config.repository.clone(),
                git_ref: config.git_ref.clone(),
            });
        }
        #[derive(Deserialize)]
        struct File {
            sha: String,
        }
        let file_path = format!("{repo}/contents/.github/workflows/temps-build.yml");
        let response = self
            .provider_request(
                &config,
                "inspect runner workflow",
                self.github(&config, reqwest::Method::GET, &file_path)
                    .query(&[("ref", &config.git_ref)]),
            )
            .await?;
        let sha = if response.status().is_success() {
            Some(self.read_json::<File>(response, 64 * 1024).await?.sha)
        } else if response.status() == reqwest::StatusCode::NOT_FOUND {
            None
        } else {
            return Err(Self::provider_error(
                &config,
                "inspect runner workflow",
                format!("HTTP {}", response.status()),
            ));
        };
        let workflow = workflow(&config);
        let mut body = serde_json::json!({"message": "ci(build): configure Temps runner", "branch": config.git_ref, "content": base64::engine::general_purpose::STANDARD.encode(workflow)});
        if let Some(sha) = sha {
            body["sha"] = sha.into();
        }
        self.checked(
            &config,
            "write runner workflow",
            self.github(&config, reqwest::Method::PUT, &file_path)
                .json(&body),
        )
        .await?;
        self.save_if_unchanged(config, Some(original_config), automatic_subject)
            .await?;
        Ok(())
    }
    async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        mut response: reqwest::Response,
        limit: usize,
    ) -> Result<T, CiError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CiError::Configuration("CI response stream failed".into()))?
        {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(CiError::Configuration(
                    "CI response exceeded its size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| CiError::Configuration("Invalid CI response".into()))
    }
    pub async fn get(&self, id: Uuid) -> Result<CiBuild, CiError> {
        CiBuild::find_by_statement(statement("SELECT id, deployment_id, state, run_id, token_id, spec_encrypted, executor_encrypted, image_repository, result, failure, expires_at FROM ci_builds WHERE id = $1", vec![id.into()])).one(self.db.as_ref()).await?.ok_or_else(|| build_error(id, "Build does not exist"))
    }
    fn spec(&self, build: &CiBuild) -> Result<BuildSpec, CiError> {
        let bytes = self
            .encryption
            .decrypt(&build.spec_encrypted)
            .map_err(|_| build_error(build.id, "Cannot decrypt build specification"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| build_error(build.id, "Invalid stored build specification"))
    }
    fn build_config(&self, build: &CiBuild) -> Result<CiExecutorSettings, CiError> {
        let bytes = self
            .encryption
            .decrypt(&build.executor_encrypted)
            .map_err(|_| build_error(build.id, "Cannot decrypt pinned executor configuration"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| build_error(build.id, "Invalid pinned executor configuration"))
    }
    pub fn context_path(&self, id: Uuid) -> PathBuf {
        self.root.join(format!("{id}.tar"))
    }
    pub async fn execute(
        &self,
        deployment_id: i32,
        request: temps_deployer::BuildRequestWithCallback,
        target: Arc<dyn temps_deployer::ImageBuilder>,
    ) -> Result<temps_deployer::BuildResult, BuilderError> {
        let config = self.config().await?.ok_or_else(|| {
            CiError::Configuration(
                "No CI builder is configured. Open Settings → Worker Nodes → CI builder".into(),
            )
        })?;
        config.validate()?;
        if config.identity.subject.is_empty() {
            return Err(CiError::Configuration("Provision the CI builder repository to configure its immutable identity before selecting CI builds".into()).into());
        }
        let id = Uuid::new_v4();
        let callback = request.log_callback;
        let (archive, spec) = tokio::task::spawn_blocking(move || {
            temps_deployer::remote::prepare_build_context(&request.request)
        })
        .await
        .map_err(|e| BuilderError::Other(format!("CI context preparation task failed: {e}")))??;
        let platform = spec
            .platform
            .clone()
            .ok_or_else(|| build_error(id, "CI builds require an explicit platform"))?;
        let spec_bytes = serde_json::to_vec(&spec).map_err(|e| build_error(id, e.to_string()))?;
        let encrypted = self
            .encryption
            .encrypt(&spec_bytes)
            .map_err(|_| build_error(id, "Cannot encrypt build specification"))?;
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(CiError::from)?;
        let expires = chrono::Utc::now()
            + chrono::Duration::seconds(i64::from(config.limits.queue_wait_seconds));
        let config_bytes = serde_json::to_vec(&config)
            .map_err(|_| build_error(id, "Cannot encode executor configuration"))?;
        let executor_encrypted = self
            .encryption
            .encrypt(&config_bytes)
            .map_err(|_| build_error(id, "Cannot encrypt pinned executor configuration"))?;
        let transaction = self.db.begin().await.map_err(CiError::from)?;
        transaction
            .execute(statement("SELECT pg_advisory_xact_lock(1528, 1)", vec![]))
            .await
            .map_err(CiError::from)?;
        #[derive(FromQueryResult)]
        struct Queued {
            count: i64,
        }
        let queued = Queued::find_by_statement(statement(
            "SELECT COUNT(*) AS count FROM ci_builds WHERE state = 'queued'",
            vec![],
        ))
        .one(&transaction)
        .await
        .map_err(CiError::from)?;
        if queued.is_some_and(|q| q.count >= 16) {
            let _ = tokio::fs::remove_file(self.context_path(id)).await;
            return Err(
                build_error(id, "CI queue is full (16 waiting builds); retry later").into(),
            );
        }
        transaction.execute(statement("INSERT INTO ci_builds (id, deployment_id, spec_encrypted, executor_encrypted, image_repository, expires_at) VALUES ($1, $2, $3, $4, $5, $6)", vec![id.into(), deployment_id.into(), encrypted.into(), executor_encrypted.into(), config.image_repository.clone().into(), expires.into()])).await.map_err(CiError::from)?;
        transaction.commit().await.map_err(CiError::from)?;
        // Persist the owner before publishing its context. Restart reconciliation
        // can then remove every context left by an interrupted copy/dispatch.
        let _context_guard = ContextGuard(self.context_path(id));
        if let Err(error) = tokio::fs::copy(archive.path(), self.context_path(id)).await {
            self.fail(id, "Cannot persist CI build context").await?;
            return Err(CiError::Io(error).into());
        }
        drop(archive);
        if let Some(callback) = &callback {
            callback(format!(
                "CI build {id}: queued for GitHub Actions ({platform})"
            ))
            .await;
        }
        let outcome = self.dispatch_and_wait(id, &config, callback.as_ref()).await;
        let _ = tokio::fs::remove_file(self.context_path(id)).await;
        let result = match outcome {
            Ok(result) => result,
            Err(error) => {
                self.fail(id, &error.to_string()).await?;
                return Err(error.into());
            }
        };
        let reference = format!("{}@{}", config.image_repository, result.digest);
        if let Some(callback) = &callback {
            callback(format!(
                "CI build {id}: verified; transferring image to deployment builder"
            ))
            .await;
        }
        target
            .fetch_registry_image(
                &reference,
                &spec.image_name,
                config.registry_credentials.clone(),
            )
            .await?;
        let actual = target.inspect_image(&spec.image_name).await?;
        if !temps_deployer::platform::platforms_match(&actual.platform, &platform) {
            return Err(BuilderError::Other(format!(
                "CI build {id}: transferred image platform {} differs from {platform}",
                actual.platform
            )));
        }
        Ok(temps_deployer::BuildResult {
            image_id: actual.id,
            image_name: spec.image_name,
            size_bytes: actual.size_bytes,
            build_duration_ms: result.duration_ms,
        })
    }
    async fn dispatch_and_wait(
        &self,
        id: Uuid,
        config: &CiExecutorSettings,
        callback: Option<&temps_deployer::LogCallback>,
    ) -> Result<RunnerResult, CiError> {
        let queue_deadline = tokio::time::Instant::now()
            + Duration::from_secs(u64::from(config.limits.queue_wait_seconds));
        loop {
            let transaction = self.db.begin().await?;
            transaction
                .execute(statement("SELECT pg_advisory_xact_lock(1528, 1)", vec![]))
                .await?;
            let changed = transaction.execute(statement("UPDATE ci_builds SET state = 'dispatched', updated_at = NOW() WHERE id = $1 AND state = 'queued' AND (SELECT COUNT(*) FROM ci_builds WHERE state IN ('dispatched', 'running') AND expires_at > NOW()) < $2", vec![id.into(), i64::from(config.limits.concurrency).into()])).await?.rows_affected();
            transaction.commit().await?;
            if changed == 1 {
                break;
            }
            self.assert_live(&self.get(id).await?).await?;
            if tokio::time::Instant::now() >= queue_deadline {
                return Err(build_error(
                    id,
                    "CI executor queue timeout; no local fallback",
                ));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let base = format!("/repos/{}/{}", config.owner, config.repository);
        self.checked(
            config,
            "dispatch runner workflow",
            self.github(
                config,
                reqwest::Method::POST,
                &format!("{base}/actions/workflows/temps-build.yml/dispatches"),
            )
            .json(
                &serde_json::json!({"ref": config.git_ref, "inputs": {"build_id": id.to_string()}}),
            ),
        )
        .await?;
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<Run>,
        }
        #[derive(Deserialize)]
        struct Run {
            id: u64,
            display_title: String,
        }
        // The handle is observed from the provider, never accepted from a runner claim.
        loop {
            let build = self.get(id).await?;
            if build.state == "failed" {
                return Err(build_error(
                    id,
                    build.failure.unwrap_or_else(|| "Build failed".into()),
                ));
            }
            self.assert_live(&build).await?;
            if build.run_id.is_none() {
                let response = self
                    .checked(
                        config,
                        "discover dispatched runner run",
                        self.github(
                            config,
                            reqwest::Method::GET,
                            &format!("{base}/actions/workflows/temps-build.yml/runs"),
                        )
                        .query(&[("per_page", "100")]),
                    )
                    .await?;
                let runs: Runs = self.read_json(response, 2 * 1024 * 1024).await?;
                if let Some(run) = runs
                    .workflow_runs
                    .into_iter()
                    .find(|run| run.display_title == format!("temps-build-{id}"))
                {
                    self.db.execute(statement("UPDATE ci_builds SET run_id = $2, updated_at = NOW() WHERE id = $1 AND run_id IS NULL AND state = 'dispatched'", vec![id.into(), run.id.to_string().into()])).await?;
                    if let Some(callback) = callback {
                        callback(format!(
                            "CI build {id}: provider run https://github.com/{}/{}/actions/runs/{}",
                            config.owner, config.repository, run.id
                        ))
                        .await;
                    }
                } else if tokio::time::Instant::now() >= queue_deadline {
                    return Err(build_error(id, "CI run was not observed before queue deadline; check that the builder shim is provisioned"));
                }
            }
            if build.state == "dispatched" && tokio::time::Instant::now() >= queue_deadline {
                return Err(build_error(
                    id,
                    "CI runner queue timeout; no local fallback",
                ));
            }
            if build.state == "complete" {
                return serde_json::from_value(
                    build
                        .result
                        .ok_or_else(|| build_error(id, "Complete build has no result"))?,
                )
                .map_err(|_| build_error(id, "Stored build result is invalid"));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    async fn assert_live(&self, build: &CiBuild) -> Result<(), CiError> {
        #[derive(FromQueryResult)]
        struct Parent {
            state: String,
            token_active: Option<bool>,
        }
        let parent = Parent::find_by_statement(statement(
            "SELECT d.state, t.is_active AS token_active FROM deployments d LEFT JOIN deployment_tokens t ON t.id = $2 WHERE d.id = $1",
            vec![build.deployment_id.into(), build.token_id.into()],
        ))
        .one(self.db.as_ref())
        .await?;
        if parent.is_none_or(|parent| {
            parent.state != "running"
                || (build.state == "running" && parent.token_active != Some(true))
        }) || (build.state != "complete" && build.expires_at <= chrono::Utc::now())
            || build.state == "cancelled"
        {
            return Err(build_error(
                build.id,
                "Deployment cancelled, interrupted by restart, build token revoked, or CI build deadline expired",
            ));
        }
        Ok(())
    }
    pub async fn claim(&self, id: Uuid, identity: &str) -> Result<RunnerWork, CiError> {
        let build = self.get(id).await?;
        if build.state != "dispatched" {
            return Err(build_error(id, "Build is already claimed or terminal"));
        }
        self.assert_live(&build).await?;
        let config = self.build_config(&build)?;
        let claims = self.verify_identity(id, &config, identity).await;
        let run_id = match claims {
            Ok(run_id) => run_id,
            Err(error) => {
                self.fail(id, "Runner identity rejected; no local fallback")
                    .await?;
                return Err(error);
            }
        };
        if build.run_id.as_deref() != Some(run_id.as_str()) {
            // Before provider discovery the runner retries; once recorded a mismatch is terminal.
            if build.run_id.is_some() {
                self.fail(
                    id,
                    "Runner run identity differs from provider run; no local fallback",
                )
                .await?;
            }
            return Err(if build.run_id.is_none() {
                CiError::NotReady {
                    build_id: id.to_string(),
                }
            } else {
                build_error(id, "Runner run identity differs from provider run")
            });
        }
        let token = format!("tbr_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let transaction = self.db.begin().await?;
        let expires = chrono::Utc::now()
            + chrono::Duration::seconds(i64::from(config.limits.build_timeout_seconds));
        let changed = transaction.execute(statement("UPDATE ci_builds SET state = 'running', expires_at = $2, updated_at = NOW() WHERE id = $1 AND state = 'dispatched' AND token_id IS NULL AND expires_at > NOW() AND EXISTS (SELECT 1 FROM deployments WHERE deployments.id = ci_builds.deployment_id AND deployments.state = 'running')", vec![id.into(), expires.into()])).await?.rows_affected();
        if changed != 1 {
            return Err(build_error(
                id,
                "Build already claimed or terminal; tokens cannot be replayed",
            ));
        }
        #[derive(FromQueryResult)]
        struct Token {
            id: i32,
        }
        let token_row = Token::find_by_statement(statement("INSERT INTO deployment_tokens (project_id, environment_id, deployment_id, name, token_hash, token_prefix, permissions, is_active, expires_at, created_at, updated_at) SELECT project_id, environment_id, id, $2, $3, 'tbr_', '[\"build:execute\"]'::jsonb, TRUE, $4, NOW(), NOW() FROM deployments WHERE id = $1 RETURNING id", vec![build.deployment_id.into(), format!("ci-build-{id}").into(), hash.into(), expires.into()])).one(&transaction).await?.ok_or_else(|| build_error(id, "Cannot mint build-scoped deployment token"))?;
        transaction
            .execute(statement(
                "UPDATE ci_builds SET token_id = $2 WHERE id = $1",
                vec![id.into(), token_row.id.into()],
            ))
            .await?;
        transaction.commit().await?;
        Ok(RunnerWork {
            token,
            spec: self.spec(&build)?,
            registry_tag: format!("{}:temps-{id}", build.image_repository),
            run_id,
            timeout_seconds: config.limits.build_timeout_seconds,
            registry_credentials: config.registry_credentials,
        })
    }
    async fn verify_identity(
        &self,
        id: Uuid,
        config: &CiExecutorSettings,
        identity: &str,
    ) -> Result<String, CiError> {
        use jsonwebtoken::{
            decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation,
        };
        let reject = || {
            CiError::Identity { build_id: id.to_string(), reason: "Signature, issuer, audience, subject or workflow did not match the configured trust policy".into() }
        };
        if identity.len() > 32 * 1024 {
            return Err(reject());
        }
        let header = decode_header(identity).map_err(|_| reject())?;
        if header.alg != Algorithm::RS256 {
            return Err(reject());
        }
        let kid = header.kid.ok_or_else(reject)?;
        let response = self
            .client
            .get(self.endpoint(config.identity.jwks_url.clone()))
            .send()
            .await
            .map_err(|_| reject())?;
        if !response.status().is_success() {
            return Err(reject());
        }
        let keys: JwkSet = self
            .read_json(response, 64 * 1024)
            .await
            .map_err(|_| reject())?;
        let key = keys.find(&kid).ok_or_else(reject)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&config.identity.issuer]);
        validation.set_audience(&[&config.identity.audience]);
        validation.sub = Some(config.identity.subject.clone());
        validation.leeway = 30;
        #[derive(Deserialize)]
        struct Claims {
            run_id: String,
            workflow_ref: String,
        }
        let claims = decode::<Claims>(
            identity,
            &DecodingKey::from_jwk(key).map_err(|_| reject())?,
            &validation,
        )
        .map_err(|_| reject())?
        .claims;
        if claims.workflow_ref
            != format!(
                "{}/{}/.github/workflows/temps-build.yml@{}",
                config.owner,
                config.repository,
                if config.git_ref.starts_with("refs/") {
                    config.git_ref.clone()
                } else {
                    format!("refs/heads/{}", config.git_ref)
                }
            )
            || claims.run_id.is_empty()
            || !claims.run_id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(reject());
        }
        Ok(claims.run_id)
    }
    pub async fn authorize(&self, id: Uuid, token: &str) -> Result<CiBuild, CiError> {
        let build = self.get(id).await?;
        self.assert_live(&build).await?;
        if build.state != "running" || token.len() != 68 {
            return Err(CiError::Identity {
                build_id: id.to_string(),
                reason: "One-build token is inactive".into(),
            });
        }
        #[derive(FromQueryResult)]
        struct Token {
            id: i32,
        }
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let token = Token::find_by_statement(statement("SELECT id FROM deployment_tokens WHERE id = $1 AND token_hash = $2 AND deployment_id = $3 AND is_active = TRUE AND expires_at > NOW() AND permissions = '[\"build:execute\"]'::jsonb", vec![build.token_id.into(), hash.into(), build.deployment_id.into()])).one(self.db.as_ref()).await?;
        if token.is_none_or(|token| Some(token.id) != build.token_id) {
            return Err(CiError::Identity {
                build_id: id.to_string(),
                reason: "Invalid one-build token".into(),
            });
        }
        Ok(build)
    }
    pub async fn complete(
        &self,
        id: Uuid,
        token: &str,
        mut result: RunnerResult,
    ) -> Result<(), CiError> {
        let build = self.authorize(id, token).await?;
        let config = self.build_config(&build)?;
        let spec = self.spec(&build)?;
        let validation = async {
            let observed = self.registry_digest(&config, id).await?;
            verify_runner_report(
                &DispatchedBuild {
                    build_id: id.to_string(),
                    deployment_id: build
                        .deployment_id
                        .ok_or_else(|| build_error(id, "Deployment was deleted"))?,
                    run_id: build.run_id,
                    platforms: spec.platform.clone().into_iter().collect(),
                    image_repository: build.image_repository,
                },
                &RunnerReport {
                    build_id: result.build_id.clone(),
                    run_id: Some(result.run_id.clone()),
                    digest: result.digest.clone(),
                    platforms: result.platforms.clone(),
                },
                &ObservedImage::RegistryDigest(observed),
            )
            .map_err(|e| build_error(id, e.to_string()))?;
            if result.image.config.is_none()
                || !spec.platform.as_ref().is_some_and(|platform| {
                    temps_deployer::platform::platforms_match(platform, &result.image.platform)
                })
            {
                return Err(build_error(
                    id,
                    "Runner omitted ImageConfig or reported another image platform",
                ));
            }
            Ok(())
        }
        .await;
        if let Err(error) = validation {
            self.fail(id, "Runner result verification rejected; no local fallback")
                .await?;
            return Err(CiError::Verification {
                build_id: id.to_string(),
                reason: error.to_string(),
            });
        }
        if let Some(config) = &mut result.image.config {
            config.env.clear();
        }
        let transaction = self.db.begin().await?;
        let value = serde_json::to_value(result).map_err(|e| build_error(id, e.to_string()))?;
        let changed = transaction.execute(statement("UPDATE ci_builds SET state = 'complete', result = $2, updated_at = NOW() WHERE id = $1 AND state = 'running' AND expires_at > NOW() AND EXISTS (SELECT 1 FROM deployments WHERE deployments.id = ci_builds.deployment_id AND deployments.state = 'running') AND EXISTS (SELECT 1 FROM deployment_tokens WHERE deployment_tokens.id = ci_builds.token_id AND deployment_tokens.is_active = TRUE AND deployment_tokens.expires_at > NOW())", vec![id.into(), value.into()])).await?.rows_affected();
        if changed != 1 {
            return Err(build_error(id, "Build was cancelled or already completed"));
        }
        transaction
            .execute(statement(
                "UPDATE deployment_tokens SET is_active = FALSE, updated_at = NOW() WHERE id = $1",
                vec![build.token_id.into()],
            ))
            .await?;
        transaction.commit().await?;
        Ok(())
    }
    async fn registry_digest(
        &self,
        config: &CiExecutorSettings,
        id: Uuid,
    ) -> Result<ImageDigest, CiError> {
        let (host, path) = config
            .image_repository
            .split_once('/')
            .ok_or_else(|| build_error(id, "Registry repository requires a host and path"))?;
        let mut request = self.client.get(self.endpoint(format!("https://{host}/v2/{path}/manifests/temps-{id}"))).header("Accept", "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json");
        if let Some(credentials) = &config.registry_credentials {
            request = request.basic_auth(
                credentials.username.clone().unwrap_or_default(),
                credentials.password.clone(),
            );
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| build_error(id, "Cannot independently resolve registry manifest"))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let challenge = response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|h| h.to_str().ok())
                .ok_or_else(|| {
                    build_error(
                        id,
                        "Registry requires authentication but supplied no Bearer challenge",
                    )
                })?;
            let (realm, service) =
                bearer_challenge(challenge, host).map_err(|reason| build_error(id, reason))?;
            let mut auth = self.client.get(realm).query(&[
                ("scope", format!("repository:{path}:pull")),
                ("service", service),
            ]);
            if let Some(credentials) = &config.registry_credentials {
                auth = auth.basic_auth(
                    credentials.username.clone().unwrap_or_default(),
                    credentials.password.clone(),
                );
            }
            #[derive(Deserialize)]
            struct RegistryToken {
                token: Option<String>,
                access_token: Option<String>,
            }
            let auth_response = auth
                .send()
                .await
                .map_err(|_| build_error(id, "Registry token exchange failed"))?;
            if !auth_response.status().is_success() {
                return Err(build_error(
                    id,
                    format!(
                        "Registry token exchange returned HTTP {}",
                        auth_response.status()
                    ),
                ));
            }
            let auth: RegistryToken = self.read_json(auth_response, 64 * 1024).await?;
            let token = auth
                .token
                .or(auth.access_token)
                .filter(|token| !token.is_empty())
                .ok_or_else(|| build_error(id, "Registry token exchange omitted a token"))?;
            response = self.client.get(self.endpoint(format!("https://{host}/v2/{path}/manifests/temps-{id}")))
                .header("Accept", "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json")
                .bearer_auth(token).send().await.map_err(|_| build_error(id, "Authenticated registry manifest resolution failed"))?;
        }
        if !response.status().is_success() {
            return Err(build_error(
                id,
                format!(
                    "Registry manifest resolution returned HTTP {}",
                    response.status()
                ),
            ));
        }
        // Hash the actual manifest bytes. A header or runner claim alone is insufficient.
        let mut bytes = Vec::new();
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| build_error(id, "Registry manifest stream failed"))?
        {
            if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                return Err(build_error(id, "Registry manifest exceeded 4 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        ImageDigest::parse(&format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
            .map_err(|e| build_error(id, e.to_string()))
    }
    pub async fn append_logs(
        &self,
        id: Uuid,
        token: &str,
        logs: temps_deployer::runner_protocol::RunnerLogs,
        log_service: &temps_logs::LogService,
    ) -> Result<(), CiError> {
        let build = self.authorize(id, token).await?;
        if logs.lines.len() > 64
            || logs.lines.iter().any(|line| line.len() > 8192)
            || logs.lines.iter().map(String::len).sum::<usize>() > 65536
        {
            return Err(build_error(id, "CI log batch exceeds 64 lines or 64 KiB"));
        }
        #[derive(FromQueryResult)]
        struct Log {
            log_id: String,
        }
        let log = Log::find_by_statement(statement("SELECT log_id FROM deployment_jobs WHERE deployment_id = $1 AND job_type = 'BuildImageJob' ORDER BY id LIMIT 1", vec![build.deployment_id.into()])).one(self.db.as_ref()).await?.ok_or_else(|| build_error(id, "Build log does not exist"))?;
        if !logs.lines.is_empty() {
            // One durable write per bounded batch, preserving line breaks in
            // the console while avoiding one archive upload per output line.
            log_service
                .log_info(&log.log_id, logs.lines.join("\n"))
                .await?;
        }
        Ok(())
    }

    pub async fn fail(&self, id: Uuid, message: &str) -> Result<(), CiError> {
        let message: String = message
            .chars()
            .filter(|c| !c.is_control())
            .take(1024)
            .collect();
        self.db.execute(statement("UPDATE ci_builds SET state = 'failed', failure = $2, updated_at = NOW() WHERE id = $1 AND state IN ('queued', 'dispatched', 'running')", vec![id.into(), message.into()])).await?;
        self.db.execute(statement("UPDATE deployment_tokens SET is_active = FALSE, updated_at = NOW() WHERE id = (SELECT token_id FROM ci_builds WHERE id = $1)", vec![id.into()])).await?;
        Ok(())
    }
    /// The existing restart policy terminates interrupted deployments. Revoke
    /// their leases and cancel provider runs before allowing any late result.
    pub async fn reconcile(&self) -> Result<(), CiError> {
        // Every disjunct is covered by the active/reconcile/orphan partial
        // indexes; completed deployment history is never scanned every tick.
        let rows = CiBuild::find_by_statement(statement("SELECT b.id, b.deployment_id, b.state, b.run_id, b.token_id, b.spec_encrypted, b.executor_encrypted, b.image_repository, b.result, b.failure, b.expires_at FROM ci_builds b LEFT JOIN deployments d ON d.id = b.deployment_id WHERE (b.deployment_id IS NULL OR b.state IN ('queued', 'dispatched', 'running') OR (b.state = 'failed' AND b.cancellation_reconciled_at IS NULL)) AND (b.deployment_id IS NULL OR (b.state IN ('queued', 'dispatched', 'running') AND (d.state != 'running' OR b.expires_at <= NOW() OR (b.state = 'running' AND NOT EXISTS (SELECT 1 FROM deployment_tokens t WHERE t.id = b.token_id AND t.is_active = TRUE AND t.expires_at > NOW())))) OR (b.state = 'failed' AND b.cancellation_reconciled_at IS NULL)) ORDER BY b.updated_at LIMIT 20", vec![])).all(self.db.as_ref()).await?;
        if rows.is_empty() {
            return Ok(());
        }
        for build in rows {
            if build.deployment_id.is_none()
                && matches!(build.state.as_str(), "complete" | "cancelled")
            {
                let _ = tokio::fs::remove_file(self.context_path(build.id)).await;
                self.db
                    .execute(statement(
                        "DELETE FROM ci_builds WHERE id = $1 AND deployment_id IS NULL",
                        vec![build.id.into()],
                    ))
                    .await?;
                continue;
            }
            let config = self.build_config(&build)?;
            self.db
                .execute(statement(
                    "UPDATE ci_builds SET updated_at = NOW() WHERE id = $1",
                    vec![build.id.into()],
                ))
                .await?;
            let terminal = if build.state == "failed" {
                "failed"
            } else {
                "cancelled"
            };
            self.fail(
                build.id,
                "Cancelled, expired, or interrupted by control-plane restart",
            )
            .await?;
            if let Some(run) = &build.run_id {
                if !run.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                let operation = format!("cancel runner run {run}");
                let response = self
                    .provider_request(
                        &config,
                        &operation,
                        self.github(
                            &config,
                            reqwest::Method::POST,
                            &format!(
                                "/repos/{}/{}/actions/runs/{run}/cancel",
                                config.owner, config.repository
                            ),
                        ),
                    )
                    .await?;
                if !response.status().is_success()
                    && response.status() != reqwest::StatusCode::CONFLICT
                    && response.status() != reqwest::StatusCode::NOT_FOUND
                {
                    let error = Self::provider_error(
                        &config,
                        &operation,
                        format!("HTTP {}; will retry", response.status()),
                    );
                    tracing::warn!(%error, build_id = %build.id, "CI cancellation remains pending");
                    continue;
                }
            }
            self.db.execute(statement("UPDATE ci_builds SET state = $2, cancellation_reconciled_at = NOW(), updated_at = NOW() WHERE id = $1 AND state = 'failed'", vec![build.id.into(), terminal.into()])).await?;
            let _ = tokio::fs::remove_file(self.context_path(build.id)).await;
            self.db
                .execute(statement(
                    "DELETE FROM ci_builds WHERE id = $1 AND deployment_id IS NULL",
                    vec![build.id.into()],
                ))
                .await?;
        }
        Ok(())
    }
}
fn bearer_challenge(
    challenge: &str,
    registry: &str,
) -> Result<(reqwest::Url, String), &'static str> {
    let parameters = challenge
        .strip_prefix("Bearer ")
        .ok_or("Registry authentication scheme is unsupported")?;
    if parameters.len() > 8192 {
        return Err("Registry Bearer challenge is too large");
    }
    let mut realm = None;
    let mut service = String::new();
    for parameter in parameters.split(',') {
        let (name, value) = parameter
            .trim()
            .split_once('=')
            .ok_or("Invalid registry Bearer challenge")?;
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or("Invalid registry Bearer challenge value")?;
        match name {
            "realm" => realm = Some(value),
            "service" => service = value.to_string(),
            "scope" => {}
            _ => {}
        }
    }
    let realm = reqwest::Url::parse(realm.ok_or("Registry Bearer challenge omitted its realm")?)
        .map_err(|_| "Invalid registry token realm")?;
    let registry_host = registry.split(':').next().ok_or("Invalid registry host")?;
    let allowed_host = realm.host_str() == Some(registry_host)
        || (registry_host == "registry-1.docker.io" && realm.host_str() == Some("auth.docker.io"));
    if realm.scheme() != "https"
        || !allowed_host
        || !realm.username().is_empty()
        || realm.password().is_some()
        || service.len() > 1024
    {
        return Err("Registry token realm must use HTTPS on the registry host; refusing to forward credentials elsewhere");
    }
    Ok((realm, service))
}

fn workflow(config: &CiExecutorSettings) -> String {
    // Only validated, non-secret installation settings appear in the shim.
    r#"# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
name: Temps build
run-name: temps-build-${{ inputs.build_id }}
on:
  workflow_dispatch:
    inputs:
      build_id:
        required: true
        type: string
permissions:
  id-token: write
  contents: read
jobs:
  build:
    runs-on: ubuntu-22.04
    timeout-minutes: %TIMEOUT%
    steps:
      - name: Enable cross-architecture builds
        uses: docker/setup-qemu-action@c7c53464625b32c7a7e944ae62b3e17d2b600130 # v3
        with:
          platforms: arm64,arm
      - name: Install pinned Temps runner
        run: |
          curl --fail --location '%ARCHIVE%' -o /tmp/temps-runner.tar.gz
          echo '%SHA%  /tmp/temps-runner.tar.gz' | sha256sum --check
          tar -xzf /tmp/temps-runner.tar.gz -C /tmp temps
      - name: Build
        env:
          TEMPS_BUILD_ID: ${{ inputs.build_id }}
          TEMPS_RUNNER_AUDIENCE: %AUDIENCE%
        run: /tmp/temps runner --control-plane '%URL%' --build-id "$TEMPS_BUILD_ID" --audience "$TEMPS_RUNNER_AUDIENCE"
"#
    .replace("%TIMEOUT%", &(config.limits.queue_wait_seconds + config.limits.build_timeout_seconds + 300).div_ceil(60).to_string())
    .replace("%AUDIENCE%", &serde_json::to_string(&config.identity.audience).unwrap_or_default())
    .replace("%ARCHIVE%", &config.runner_archive_url)
    .replace("%SHA%", &config.runner_archive_sha256)
    .replace("%URL%", config.control_plane_url.trim_end_matches('/'))
}

pub struct CiImageBuilder {
    pub service: CiDispatchService,
    pub deployment_id: i32,
    pub target: Arc<dyn temps_deployer::ImageBuilder>,
}
#[async_trait::async_trait]
impl temps_deployer::ImageBuilder for CiImageBuilder {
    async fn build_image(
        &self,
        request: temps_deployer::BuildRequest,
    ) -> Result<temps_deployer::BuildResult, BuilderError> {
        self.service
            .execute(
                self.deployment_id,
                temps_deployer::BuildRequestWithCallback {
                    request,
                    log_callback: None,
                },
                self.target.clone(),
            )
            .await
    }
    async fn build_image_with_callback(
        &self,
        request: temps_deployer::BuildRequestWithCallback,
    ) -> Result<temps_deployer::BuildResult, BuilderError> {
        self.service
            .execute(self.deployment_id, request, self.target.clone())
            .await
    }
    async fn import_image(&self, image_path: PathBuf, tag: &str) -> Result<String, BuilderError> {
        self.target.import_image(image_path, tag).await
    }
    async fn import_image_stream(
        &self,
        stream: temps_deployer::ImageImportStream,
        tag: &str,
    ) -> Result<String, BuilderError> {
        self.target.import_image_stream(stream, tag).await
    }
    async fn save_image(
        &self,
        image_name: &str,
        output_path: &std::path::Path,
    ) -> Result<(), BuilderError> {
        self.target.save_image(image_name, output_path).await
    }
    async fn export_image_stream(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::ImageImportStream, BuilderError> {
        self.target.export_image_stream(image_name).await
    }
    async fn export_path_stream(
        &self,
        image_name: &str,
        source_path: &str,
    ) -> Result<temps_deployer::ImageImportStream, BuilderError> {
        self.target
            .export_path_stream(image_name, source_path)
            .await
    }
    async fn scan_image_vulnerabilities(
        &self,
        image_name: &str,
        expected_image_id: Option<&str>,
    ) -> Result<Option<temps_deployer::image_scan::ImageScanReport>, BuilderError> {
        self.target
            .scan_image_vulnerabilities(image_name, expected_image_id)
            .await
    }
    async fn extract_from_image(
        &self,
        image_name: &str,
        source_path: &str,
        destination_path: &std::path::Path,
    ) -> Result<(), BuilderError> {
        self.target
            .extract_from_image(image_name, source_path, destination_path)
            .await
    }
    async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
        self.target.list_images().await
    }
    async fn remove_image(&self, image_name: &str) -> Result<(), BuilderError> {
        self.target.remove_image(image_name).await
    }
    async fn inspect_image(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::ImageInfo, BuilderError> {
        self.target.inspect_image(image_name).await
    }
    async fn image_identity(
        &self,
        image_name: &str,
    ) -> Result<temps_deployer::LocalImageIdentity, BuilderError> {
        self.target.image_identity(image_name).await
    }
    async fn ensure_platform_discovered(&self) -> Option<String> {
        self.target.ensure_platform_discovered().await
    }
    fn get_native_platform(&self) -> String {
        self.target.get_native_platform()
    }
    fn discovered_platform(&self) -> Option<String> {
        self.target.discovered_platform()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use rsa::{
        pkcs8::{EncodePrivateKey, LineEnding},
        traits::PublicKeyParts,
    };
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };
    use temps_deployer::{
        BuildRequest, BuildRequestWithCallback, BuildResult, ImageBuilder, ImageConfig, ImageInfo,
    };

    fn settings() -> CiExecutorSettings {
        CiExecutorSettings {
            owner: "builder-owner".into(),
            repository: "builder-repo".into(),
            organization: false,
            git_ref: "main".into(),
            control_plane_url: "https://console.example.test".into(),
            runner_archive_url: "https://releases.example.test/temps.tar.gz".into(),
            runner_archive_sha256: "a".repeat(64),
            identity: TrustedIdentityIssuer {
                issuer: "https://issuer.example.test".into(),
                jwks_url: "https://issuer.example.test/jwks".into(),
                audience: "temps-build".into(),
                subject: "repo:builder-owner/builder-repo:ref:refs/heads/main".into(),
            },
            image_repository: "registry.example.test/builds/app".into(),
            limits: BuildLimits {
                queue_wait_seconds: 30,
                build_timeout_seconds: 30,
                concurrency: 1,
            },
            dispatch_token: Some("synthetic-api-token".into()),
            registry_credentials: None,
        }
    }
    #[test]
    fn executor_settings_reject_shell_and_repository_injection() {
        let good = settings();
        assert!(good.validate().is_ok());
        for value in [
            "https://releases.example.test/'$(echo unsafe)",
            "http://releases.example.test/file",
            "https://user:secret@releases.example.test/file",
        ] {
            let mut config = good.clone();
            config.runner_archive_url = value.into();
            assert!(config.validate().is_err());
        }
        for name in ["../repo", "..", "owner/repo", "repo?token=secret"] {
            let mut config = good.clone();
            config.repository = name.into();
            assert!(config.validate().is_err());
        }
        let mut config = good.clone();
        config.image_repository = "registry.example.test/builds/app:tag".into();
        assert!(config.validate().is_err());
        let shim = workflow(&good);
        assert!(shim.contains("run-name: temps-build-${{ inputs.build_id }}"));
        assert!(shim.contains("sha256sum --check"));
        assert!(!shim.contains("synthetic-api-token"));
        assert!(!shim.contains("registry_credentials"));
        assert!(shim.contains("--audience"));
        assert!(shim.contains("docker/setup-qemu-action@c7c53464625b32c7a7e944ae62b3e17d2b600130"));
    }
    #[test]
    fn registry_auth_refuses_cross_host_or_cleartext_credential_forwarding() {
        assert!(bearer_challenge(
            "Bearer realm=\"https://registry.example.test/token\",service=\"registry\"",
            "registry.example.test"
        )
        .is_ok());
        assert!(bearer_challenge(
            "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\"",
            "registry-1.docker.io"
        )
        .is_ok());
        for challenge in [
            "Bearer realm=\"https://attacker.example.test/token\"",
            "Bearer realm=\"http://registry.example.test/token\"",
            "Basic realm=\"registry\"",
            "Bearer service=\"registry\"",
        ] {
            assert!(bearer_challenge(challenge, "registry.example.test").is_err());
        }
    }
    fn image() -> ImageInfo {
        ImageInfo {
            id: format!("sha256:{}", "c".repeat(64)),
            architecture: "amd64".into(),
            os: "linux".into(),
            platform: "linux/amd64".into(),
            size_bytes: 42,
            tags: vec![],
            created: None,
            working_dir: Some("/app".into()),
            config: Some(ImageConfig {
                exposed_ports: vec![8080],
                env: vec!["SYNTHETIC_VALUE=do-not-persist".into()],
                ..Default::default()
            }),
        }
    }
    #[derive(Default)]
    struct ArtifactTarget {
        builds: AtomicUsize,
        pulls: Mutex<Vec<(String, String)>>,
    }
    #[async_trait::async_trait]
    impl ImageBuilder for ArtifactTarget {
        async fn build_image(&self, _: BuildRequest) -> Result<BuildResult, BuilderError> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Err(BuilderError::Other(
                "Local build must never run for CI".into(),
            ))
        }
        async fn build_image_with_callback(
            &self,
            request: BuildRequestWithCallback,
        ) -> Result<BuildResult, BuilderError> {
            self.build_image(request.request).await
        }
        async fn fetch_registry_image(
            &self,
            reference: &str,
            tag: &str,
            _: Option<temps_deployer::remote::RemotePullCredentials>,
        ) -> Result<(), BuilderError> {
            self.pulls
                .lock()
                .unwrap()
                .push((reference.into(), tag.into()));
            Ok(())
        }
        async fn inspect_image(&self, _: &str) -> Result<ImageInfo, BuilderError> {
            Ok(image())
        }
        async fn import_image(&self, _: PathBuf, _: &str) -> Result<String, BuilderError> {
            unreachable!()
        }
        async fn save_image(&self, _: &str, _: &std::path::Path) -> Result<(), BuilderError> {
            unreachable!()
        }
        async fn extract_from_image(
            &self,
            _: &str,
            _: &str,
            _: &std::path::Path,
        ) -> Result<(), BuilderError> {
            unreachable!()
        }
        async fn list_images(&self) -> Result<Vec<String>, BuilderError> {
            unreachable!()
        }
        async fn remove_image(&self, _: &str) -> Result<(), BuilderError> {
            unreachable!()
        }
        fn get_native_platform(&self) -> String {
            "linux/amd64".into()
        }
    }
    #[derive(Clone)]
    struct Provider {
        build_id: Arc<Mutex<Option<Uuid>>>,
        cancels: Arc<AtomicUsize>,
        cancel_failure: Arc<AtomicBool>,
        jwks: serde_json::Value,
        created: Arc<AtomicBool>,
        claim_template: Arc<Mutex<Option<serde_json::Value>>>,
        shim: Arc<Mutex<Option<serde_json::Value>>>,
    }
    async fn mock_provider(
        axum::extract::State(state): axum::extract::State<Provider>,
        request: axum::extract::Request,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let path = request.uri().path().to_string();
        if path == "/user" {
            return axum::Json(serde_json::json!({"login": "builder-owner"})).into_response();
        }
        if path == "/user/repos" {
            let body = axum::body::to_bytes(request.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["private"], true);
            state.created.store(true, Ordering::SeqCst);
            return axum::http::StatusCode::CREATED.into_response();
        }
        if path.starts_with("/repos/") && path.split('/').count() == 4 {
            if !state.created.load(Ordering::SeqCst) {
                return axum::http::StatusCode::NOT_FOUND.into_response();
            }
            return axum::Json(
                serde_json::json!({"id": if path.ends_with("/builder-repo") {73} else {74}, "full_name": path.trim_start_matches("/repos/")}),
            )
            .into_response();
        }
        if path.ends_with("/actions/oidc/customization/sub") {
            let body = axum::body::to_bytes(request.into_body(), 4096)
                .await
                .unwrap();
            *state.claim_template.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
            return axum::http::StatusCode::NO_CONTENT.into_response();
        }
        if path.contains("/contents/") && request.method() == axum::http::Method::PUT {
            let body = axum::body::to_bytes(request.into_body(), 8192)
                .await
                .unwrap();
            *state.shim.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
            return axum::http::StatusCode::CREATED.into_response();
        }
        if path == "/jwks" {
            return axum::Json(state.jwks).into_response();
        }
        if path.ends_with("/dispatches") {
            let body = axum::body::to_bytes(request.into_body(), 4096)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            *state.build_id.lock().unwrap() = Some(
                value["inputs"]["build_id"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap(),
            );
            return axum::http::StatusCode::NO_CONTENT.into_response();
        }
        if path.ends_with("/runs") {
            let id = state.build_id.lock().unwrap().unwrap();
            return axum::Json(serde_json::json!({"workflow_runs": [{"id": 42, "display_title": format!("temps-build-{id}")}]})).into_response();
        }
        if path.ends_with("/cancel") {
            state.cancels.fetch_add(1, Ordering::SeqCst);
            if state.cancel_failure.load(Ordering::SeqCst) {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            return axum::http::StatusCode::ACCEPTED.into_response();
        }
        if path.starts_with("/v2/") {
            return (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "application/vnd.oci.image.manifest.v1+json",
                )],
                "synthetic-manifest",
            )
                .into_response();
        }
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
    fn identity_key() -> (jsonwebtoken::EncodingKey, serde_json::Value) {
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let public = rsa::RsaPublicKey::from(&private);
        let jwk = serde_json::json!({"keys": [{"kty": "RSA", "kid": "test-key", "alg": "RS256", "use": "sig", "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()), "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be())}]});
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(
            private.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes(),
        )
        .unwrap();
        (key, jwk)
    }
    fn identity(
        config: &CiExecutorSettings,
        key: &jsonwebtoken::EncodingKey,
        change: Option<(&str, serde_json::Value)>,
    ) -> String {
        let mut value = serde_json::json!({"iss": config.identity.issuer, "aud": config.identity.audience, "sub": config.identity.subject, "exp": chrono::Utc::now().timestamp()+300, "run_id": "42", "workflow_ref": "builder-owner/builder-repo/.github/workflows/temps-build.yml@refs/heads/main"});
        if let Some((name, change)) = change {
            value[name] = change;
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("test-key".into());
        jsonwebtoken::encode(&header, &value, key).unwrap()
    }
    async fn seed(
        service: &CiDispatchService,
        config: &CiExecutorSettings,
        state: &str,
        token: Option<&str>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let spec = BuildSpec {
            version: 1,
            image_name: "app:build".into(),
            dockerfile: "Dockerfile".into(),
            platform: Some("linux/amd64".into()),
            cache_namespace: None,
            build_args: Default::default(),
        };
        let spec = service
            .encryption
            .encrypt(&serde_json::to_vec(&spec).unwrap())
            .unwrap();
        let executor = service
            .encryption
            .encrypt(&serde_json::to_vec(config).unwrap())
            .unwrap();
        service.db.execute(statement("INSERT INTO ci_builds (id, deployment_id, state, run_id, spec_encrypted, executor_encrypted, image_repository, expires_at) VALUES ($1, 1, $2, '42', $3, $4, $5, NOW()+INTERVAL '1 hour')", vec![id.into(), state.into(), spec.into(), executor.into(), config.image_repository.clone().into()])).await.unwrap();
        if let Some(token) = token {
            service.db.execute(statement("INSERT INTO deployment_tokens (project_id, environment_id, deployment_id, name, token_hash, token_prefix, permissions, is_active, expires_at, created_at, updated_at) VALUES (1, 1, 1, $1, $2, 'tbr_', '[\"build:execute\"]'::jsonb, TRUE, NOW()+INTERVAL '1 hour', NOW(), NOW())", vec![format!("ci-build-{id}").into(), hex::encode(Sha256::digest(token.as_bytes())).into()])).await.unwrap();
            service.db.execute(statement("UPDATE ci_builds SET token_id = (SELECT id FROM deployment_tokens WHERE name = $2) WHERE id = $1", vec![id.into(), format!("ci-build-{id}").into()])).await.unwrap();
        }
        id
    }
    #[tokio::test]
    async fn ci_dispatch_integrates_claim_registry_verification_token_replay_and_reconciliation() {
        let db = match temps_database::test_utils::TestDatabase::new().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Database unavailable; skipping CI integration: {error}");
                return;
            }
        };
        let conn = db.db.clone();
        conn.execute_unprepared("CREATE TABLE deployments (id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, environment_id INTEGER NOT NULL, state TEXT NOT NULL);
            INSERT INTO deployments VALUES (1, 1, 1, 'running');
            CREATE TABLE deployment_jobs (id INTEGER PRIMARY KEY, deployment_id INTEGER, job_type TEXT, log_id TEXT);
            INSERT INTO deployment_jobs VALUES (1, 1, 'BuildImageJob', 'ci-test-build');
            CREATE TABLE deployment_tokens (id SERIAL PRIMARY KEY, project_id INTEGER, environment_id INTEGER, deployment_id INTEGER REFERENCES deployments(id) ON DELETE CASCADE, name TEXT, token_hash TEXT, token_prefix TEXT, permissions JSONB, is_active BOOLEAN, expires_at TIMESTAMPTZ, created_at TIMESTAMPTZ, updated_at TIMESTAMPTZ);").await.unwrap();
        use sea_orm_migration::MigrationTrait;
        temps_migrations::CiBuildDispatchMigration
            .up(&sea_orm_migration::SchemaManager::new(conn.as_ref()))
            .await
            .unwrap();
        let manager = sea_orm_migration::SchemaManager::new(conn.as_ref());
        // Upgrade the original CASCADE schema, including rollback/reapply.
        temps_migrations::RetainCiCancellationMigration
            .up(&manager)
            .await
            .unwrap();
        temps_migrations::RetainCiCancellationMigration
            .down(&manager)
            .await
            .unwrap();
        temps_migrations::RetainCiCancellationMigration
            .up(&manager)
            .await
            .unwrap();
        temps_migrations::RetainCiCancellationMigration
            .up(&manager)
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let encryption = Arc::new(EncryptionService::new_from_password(
            "synthetic-ci-test-only",
        ));
        let mut service =
            CiDispatchService::new(conn.clone(), encryption, root.path().into()).unwrap();
        let (key, jwks) = identity_key();
        let provider = Provider {
            build_id: Arc::new(Mutex::new(None)),
            cancels: Arc::new(AtomicUsize::new(0)),
            cancel_failure: Arc::new(AtomicBool::new(false)),
            jwks,
            created: Arc::new(AtomicBool::new(false)),
            claim_template: Default::default(),
            shim: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        service.test_origin = Some(format!("http://{}", listener.local_addr().unwrap()));
        let app = axum::Router::new()
            .fallback(mock_provider)
            .with_state(provider.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        struct Stop(tokio::task::JoinHandle<()>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _server = Stop(server);
        let mut config = settings();
        config.identity.subject.clear();
        service.save(config.clone()).await.unwrap();
        assert!(!service.status().await.unwrap().configured);
        let mut wrong_owner = config.clone();
        wrong_owner.owner = "unmatched-owner".into();
        service.save(wrong_owner).await.unwrap();
        assert!(service.provision().await.is_err());
        assert!(
            !provider.created.load(Ordering::SeqCst),
            "a mismatched owner must not create a repository in the token's account"
        );
        service.save(config.clone()).await.unwrap();
        service.provision().await.unwrap();
        config = service.config().await.unwrap().unwrap();
        assert_eq!(
            config.identity.subject,
            "repository_id:73:ref:refs/heads/main"
        );
        assert!(provider.created.load(Ordering::SeqCst));
        assert_eq!(
            provider.claim_template.lock().unwrap().as_ref().unwrap()["include_claim_keys"],
            serde_json::json!(["repository_id", "ref"])
        );
        assert!(provider.shim.lock().unwrap().as_ref().unwrap()["content"]
            .as_str()
            .is_some());
        let status = service.status().await.unwrap();
        assert!(status.configured);
        assert!(status.settings.unwrap().dispatch_token.is_none());
        // Reprovisioning does not lose provenance, and changing either source
        // invalidates managed identity before the next deployment can dispatch.
        service.provision().await.unwrap();
        for (repository, git_ref, expected) in [
            (
                "builder-repo",
                "next",
                "repository_id:73:ref:refs/heads/next",
            ),
            (
                "builder-next",
                "main",
                "repository_id:74:ref:refs/heads/main",
            ),
        ] {
            let mut changed = service.config().await.unwrap().unwrap();
            changed.repository = repository.into();
            changed.git_ref = git_ref.into();
            service.save(changed).await.unwrap();
            assert!(!service.status().await.unwrap().configured);
            service.provision().await.unwrap();
            assert_eq!(
                service.config().await.unwrap().unwrap().identity.subject,
                expected
            );
        }
        let mut custom = service.config().await.unwrap().unwrap();
        custom.identity.subject = "operator-custom-subject".into();
        service.save(custom.clone()).await.unwrap();
        custom.repository = "builder-repo".into();
        custom.git_ref = "main".into();
        service.save(custom).await.unwrap();
        service.provision().await.unwrap();
        assert_eq!(
            service.config().await.unwrap().unwrap().identity.subject,
            "operator-custom-subject"
        );
        // Restore managed settings for the identity and dispatch checks below.
        config.identity.subject.clear();
        service.save(config).await.unwrap();
        service.provision().await.unwrap();
        config = service.config().await.unwrap().unwrap();
        // The crypto validator checks each trust dimension against the pinned policy.
        for (field, value) in [
            ("iss", serde_json::json!("https://other.example.test")),
            ("aud", serde_json::json!("other")),
            ("sub", serde_json::json!("other")),
            (
                "workflow_ref",
                serde_json::json!("other/workflow@refs/heads/main"),
            ),
            ("exp", serde_json::json!(1)),
        ] {
            assert!(matches!(
                service
                    .verify_identity(
                        Uuid::new_v4(),
                        &config,
                        &identity(&config, &key, Some((field, value)))
                    )
                    .await,
                Err(CiError::Identity { .. })
            ));
        }
        let context = tempfile::tempdir().unwrap();
        std::fs::write(
            context.path().join("Dockerfile"),
            "FROM scratch\nEXPOSE 8080\n",
        )
        .unwrap();
        let target = Arc::new(ArtifactTarget::default());
        let build_service = service.clone();
        let build_target = target.clone();
        let request = BuildRequest {
            context_path: context.path().into(),
            dockerfile_path: None,
            image_name: "app:build".into(),
            build_args: Default::default(),
            build_args_buildkit: Default::default(),
            platform: Some("linux/amd64".into()),
            log_path: context.path().join("build.log"),
            cache_from: vec![],
        };
        let mut execution = tokio::spawn(async move {
            build_service
                .execute(
                    1,
                    BuildRequestWithCallback {
                        request,
                        log_callback: None,
                    },
                    build_target,
                )
                .await
        });
        let id = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if execution.is_finished() {
                    panic!(
                        "Dispatch ended before the provider received it: {:?}",
                        (&mut execution).await
                    );
                }
                if let Some(id) = *provider.build_id.lock().unwrap() {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let jwt = identity(&config, &key, None);
        let work = loop {
            match service.claim(id, &jwt).await {
                Ok(work) => break work,
                Err(CiError::NotReady { .. }) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(error) => panic!("claim failed: {error}"),
            }
        };
        assert!(service.context_path(id).is_file());
        assert!(service.claim(id, &jwt).await.is_err());
        let logs = temps_logs::LogService::new(root.path().join("logs"));
        service
            .append_logs(
                id,
                &work.token,
                temps_deployer::runner_protocol::RunnerLogs {
                    lines: vec!["Building image".into(), "Image built".into()],
                },
                &logs,
            )
            .await
            .unwrap();
        let entries = logs.get_structured_logs("ci-test-build").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "Building image\nImage built");
        assert!(service
            .append_logs(
                id,
                &work.token,
                temps_deployer::runner_protocol::RunnerLogs {
                    lines: vec!["too many".into(); 65],
                },
                &logs
            )
            .await
            .is_err());
        let digest = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(b"synthetic-manifest"))
        );
        let result = RunnerResult {
            build_id: id.to_string(),
            run_id: "42".into(),
            digest: digest.clone(),
            platforms: vec!["linux/amd64".into()],
            image: image(),
            duration_ms: 12,
        };
        let mut changed_executor = config.clone();
        changed_executor.identity.subject = "different-policy".into();
        changed_executor.image_repository = "registry.example.test/other/app".into();
        service.save(changed_executor).await.unwrap();
        // In-flight builds retain the original issuer, subject and registry namespace.
        service
            .complete(id, &work.token, result.clone())
            .await
            .unwrap();
        // A result accepted within its deadline stays valid when the next
        // polling tick happens after the deadline. Parent cancellation still
        // fences it through assert_live.
        conn.execute(statement(
            "UPDATE ci_builds SET expires_at = NOW() - INTERVAL '1 second' WHERE id = $1",
            vec![id.into()],
        ))
        .await
        .unwrap();
        assert!(service.authorize(id, &work.token).await.is_err());
        assert!(service
            .complete(id, &work.token, result.clone())
            .await
            .is_err());
        let built = tokio::time::timeout(Duration::from_secs(30), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(built.image_name, "app:build");
        assert_eq!(target.builds.load(Ordering::SeqCst), 0);
        assert_eq!(
            target.pulls.lock().unwrap().as_slice(),
            &[(
                format!("{}@{digest}", config.image_repository),
                "app:build".into()
            )]
        );
        assert!(!service
            .get(id)
            .await
            .unwrap()
            .result
            .unwrap()
            .to_string()
            .contains("do-not-persist"));
        let token = format!("tbr_{}", "b".repeat(64));
        let wrong = seed(&service, &config, "running", Some(&token)).await;
        assert!(service.authorize(wrong, &work.token).await.is_err());
        let mut bad_result = result.clone();
        bad_result.build_id = wrong.to_string();
        bad_result.digest = format!("sha256:{}", "e".repeat(64));
        assert!(service.complete(wrong, &token, bad_result).await.is_err());
        assert_eq!(service.get(wrong).await.unwrap().state, "failed");
        assert!(service.authorize(wrong, &token).await.is_err());
        let rejected = seed(&service, &config, "dispatched", None).await;
        assert!(service
            .claim(
                rejected,
                &identity(&config, &key, Some(("sub", "untrusted".into())))
            )
            .await
            .is_err());
        assert_eq!(service.get(rejected).await.unwrap().state, "failed");
        let cancelled = seed(&service, &config, "running", Some(&token)).await;
        conn.execute_unprepared("UPDATE deployments SET state = 'cancelled' WHERE id = 1")
            .await
            .unwrap();
        assert!(service.authorize(cancelled, &token).await.is_err());
        service.reconcile().await.unwrap();
        assert_eq!(service.get(cancelled).await.unwrap().state, "cancelled");
        assert!(provider.cancels.load(Ordering::SeqCst) >= 1);
        assert_eq!(target.pulls.lock().unwrap().len(), 1);
        // Deleting a project cascades through deployments and deployment tokens.
        // Known provider runs must survive both deletes and a provider outage.
        conn.execute_unprepared("UPDATE deployments SET state = 'running' WHERE id = 1")
            .await
            .unwrap();
        let orphan = seed(&service, &config, "running", Some(&token)).await;
        std::fs::create_dir_all(service.context_path(orphan).parent().unwrap()).unwrap();
        std::fs::write(service.context_path(orphan), "private-build-context").unwrap();
        conn.execute_unprepared("DELETE FROM deployments WHERE id = 1")
            .await
            .unwrap();
        let tombstone = service.get(orphan).await.unwrap();
        assert!(tombstone.token_id.is_none());
        assert!(tombstone.deployment_id.is_none());
        let rollback = temps_migrations::RetainCiCancellationMigration
            .down(&manager)
            .await
            .unwrap_err();
        assert!(rollback.to_string().contains("orphan builds remain"));
        assert!(service.authorize(orphan, &token).await.is_err());
        provider.cancel_failure.store(true, Ordering::SeqCst);
        let attempts = provider.cancels.load(Ordering::SeqCst);
        service.reconcile().await.unwrap();
        assert!(provider.cancels.load(Ordering::SeqCst) > attempts);
        assert_eq!(service.get(orphan).await.unwrap().state, "failed");
        assert!(service.context_path(orphan).exists());
        provider.cancel_failure.store(false, Ordering::SeqCst);
        service.reconcile().await.unwrap();
        assert!(
            service.get(orphan).await.is_err(),
            "successful orphan cleanup removes encrypted spec/executor secrets"
        );
        assert!(!service.context_path(orphan).exists());
    }

    #[tokio::test]
    async fn provider_errors_include_operation_repository_and_safe_transport_cause() {
        let service = CiDispatchService::new(
            Arc::new(sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Arc::new(EncryptionService::new_from_password("synthetic-error-test")),
            PathBuf::new(),
        )
        .unwrap();
        let config = settings();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = service
            .checked(
                &config,
                "write runner workflow",
                service
                    .client
                    .get(format!(
                        "http://test-user:transport-password@{address}/private?token=query-secret"
                    ))
                    .bearer_auth("authorization-secret"),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("write runner workflow"), "{error}");
        assert!(error.contains("builder-owner/builder-repo"), "{error}");
        assert!(
            error.contains("connect") || error.contains("refused"),
            "{error}"
        );
        for secret in [
            "transport-password",
            "query-secret",
            "authorization-secret",
            "test-user",
            &address.to_string(),
        ] {
            assert!(!error.contains(secret), "{error}");
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().fallback(|| async {
                    (axum::http::StatusCode::FORBIDDEN, "provider-secret-body")
                }),
            )
            .await
            .unwrap();
        });
        let error = service
            .checked(
                &config,
                "create private repository",
                service.client.get(format!("http://{address}/")),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("create private repository")
                && error.contains("builder-owner/builder-repo")
                && error.contains("403"),
            "{error}"
        );
        assert!(!error.contains("provider-secret-body"));
        server.abort();
    }
}
