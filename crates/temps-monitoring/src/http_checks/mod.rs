// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistence and scheduling adapter for the provider-independent verification crate.
pub mod handlers;
mod transport;
mod variable_history;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::{
    notifications::{
        NotificationData, NotificationPriority, NotificationService, NotificationType,
    },
    EncryptionService,
};
use temps_credential_checks::{
    Candidate, CatalogDetector, CheckStatus, CredentialDetector, CredentialVerifier, HttpCheckSpec,
    HttpCheckTransport, HttpCredentialVerifier, VerificationResult,
};
use temps_entities::{env_vars, http_checks, projects};
use utoipa::ToSchema;
pub use variable_history::{VariableHistoryDetails, VariableHistoryEntry, VariableHistoryList};

#[derive(Debug, thiserror::Error)]
pub enum HttpChecksError {
    #[error("HTTP check {id} was not found in project {project_id}")]
    NotFound { project_id: i32, id: i32 },
    #[error("Invalid HTTP check: {reason}")]
    Invalid { reason: String },
    #[error("HTTP check {id} is already running or was checked within the last 30 seconds")]
    Busy { id: i32 },
    #[error("Database operation '{operation}' failed for HTTP checks in project {project_id}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
    #[error("Credential storage operation failed in project {project_id}")]
    Encryption { project_id: i32 },
    #[error("HTTP check {id} contains unreadable stored configuration or results")]
    Stored { id: i32 },
    #[error("History entry {entry_id} for variable {env_var_id} in project {project_id} contains invalid stored details")]
    HistoryStored {
        project_id: i32,
        env_var_id: i32,
        entry_id: i64,
    },
}
fn db_error(project_id: i32, operation: &'static str, source: DbErr) -> HttpChecksError {
    HttpChecksError::Database {
        project_id,
        operation,
        source,
    }
}

/// Credentials and recipe headers are write-only and encrypted at rest.
#[derive(Deserialize, ToSchema)]
pub struct SaveHttpCheck {
    pub name: String,
    pub env_var_id: Option<i32>,
    pub credential: Option<String>,
    pub spec: HttpCheckSpec,
    #[serde(default = "daily")]
    pub interval_seconds: i32,
    #[serde(default = "enabled")]
    pub enabled: bool,
}
fn daily() -> i32 {
    86400
}
fn enabled() -> bool {
    true
}
#[derive(Debug, Serialize, ToSchema)]
pub struct HttpCheckView {
    pub id: i32,
    pub project_id: i32,
    pub env_var_id: Option<i32>,
    pub name: String,
    pub automatic_provider: Option<String>,
    pub enabled: bool,
    pub interval_seconds: i32,
    #[schema(value_type=String,format=DateTime)]
    pub next_check_at: DateTime<Utc>,
    #[schema(value_type=Option<String>,format=DateTime)]
    pub last_checked_at: Option<DateTime<Utc>>,
    pub result: Option<VerificationResult>,
}
impl TryFrom<http_checks::Model> for HttpCheckView {
    type Error = HttpChecksError;
    fn try_from(m: http_checks::Model) -> Result<Self, Self::Error> {
        let result = m
            .last_result
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| HttpChecksError::Stored { id: m.id })?;
        Ok(Self {
            id: m.id,
            project_id: m.project_id,
            env_var_id: m.env_var_id,
            name: m.name,
            automatic_provider: m.automatic_provider,
            enabled: m.enabled,
            interval_seconds: m.interval_seconds,
            next_check_at: m.next_check_at,
            last_checked_at: m.last_checked_at,
            result,
        })
    }
}
#[derive(Serialize, ToSchema)]
pub struct HttpCheckList {
    pub items: Vec<HttpCheckView>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}
#[derive(Serialize, ToSchema)]
pub struct DetectionView {
    pub env_var_id: i32,
    pub candidates: Vec<Candidate>,
    pub detection_rule_count: usize,
}

#[derive(Serialize, ToSchema)]
pub struct HttpChecksCapabilities {
    pub detection_rule_count: usize,
    pub alerts_configured: bool,
    pub alerts_setup_path: String,
}
#[derive(Deserialize, ToSchema)]
pub struct SetHttpCheckEnabled {
    pub enabled: bool,
}

pub struct HttpChecksService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    notifications: Arc<dyn NotificationService>,
    detector: CatalogDetector,
    transport: Arc<dyn HttpCheckTransport>,
    permits: Arc<tokio::sync::Semaphore>,
}
impl HttpChecksService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        notifications: Arc<dyn NotificationService>,
    ) -> Result<Self, temps_credential_checks::DetectionError> {
        Ok(Self {
            db,
            encryption,
            notifications,
            detector: CatalogDetector::bundled()?,
            transport: Arc::new(transport::SafeHttpTransport),
            permits: Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }
    pub async fn capabilities(&self) -> HttpChecksCapabilities {
        HttpChecksCapabilities {
            detection_rule_count: self.detector.rule_count(),
            alerts_configured: self.notifications.is_configured().await.unwrap_or(false),
            alerts_setup_path: "/settings/notifications".into(),
        }
    }
    pub async fn set_enabled(
        &self,
        project_id: i32,
        id: i32,
        enabled: bool,
    ) -> Result<HttpCheckView, HttpChecksError> {
        self.row(project_id, id).await?;
        http_checks::Entity::update_many()
            .col_expr(http_checks::Column::Enabled, Expr::value(enabled))
            .col_expr(http_checks::Column::NextCheckAt, Expr::value(Utc::now()))
            .col_expr(http_checks::Column::LeaseToken, Expr::value(None::<String>))
            .col_expr(
                http_checks::Column::LeaseUntil,
                Expr::value(None::<DateTime<Utc>>),
            )
            .filter(http_checks::Column::Id.eq(id))
            .filter(http_checks::Column::ProjectId.eq(project_id))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "set enabled", e))?;
        self.row(project_id, id).await?.try_into()
    }
    pub async fn list(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<HttpCheckList, HttpChecksError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let query = http_checks::Entity::find()
            .filter(http_checks::Column::ProjectId.eq(project_id))
            .order_by_desc(http_checks::Column::CreatedAt)
            .order_by_desc(http_checks::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = query
            .num_items()
            .await
            .map_err(|e| db_error(project_id, "count", e))?;
        let items = query
            .fetch_page(page - 1)
            .await
            .map_err(|e| db_error(project_id, "list", e))?
            .into_iter()
            .map(HttpCheckView::try_from)
            .collect::<Result<_, _>>()?;
        Ok(HttpCheckList {
            items,
            total,
            page,
            page_size,
        })
    }
    async fn row(&self, project_id: i32, id: i32) -> Result<http_checks::Model, HttpChecksError> {
        http_checks::Entity::find_by_id(id)
            .filter(http_checks::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "find", e))?
            .ok_or(HttpChecksError::NotFound { project_id, id })
    }
    async fn env_credential(
        &self,
        project_id: i32,
        id: i32,
    ) -> Result<(String, String, bool), HttpChecksError> {
        let env = env_vars::Entity::find_by_id(id)
            .filter(env_vars::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "read credential", e))?
            .ok_or(HttpChecksError::Invalid {
                reason: "The environment variable does not belong to this project.".into(),
            })?;
        let value = if env.is_encrypted {
            self.encryption
                .decrypt_string(&env.value)
                .map_err(|_| HttpChecksError::Encryption { project_id })?
        } else {
            env.value
        };
        Ok((env.key, value, env.is_secret))
    }
    // Write-only variables may only go to their value-verified public issuer.
    // Validate again at execution so rotation or legacy saved checks cannot bypass this.
    fn validate_credential_destination(
        &self,
        key: &str,
        value: &str,
        restricted: bool,
        spec: &HttpCheckSpec,
    ) -> Result<(), HttpChecksError> {
        if !restricted {
            return Ok(());
        }
        let preset =
            temps_credential_checks::automatic_preset(&self.detector.detect(key, value), value);
        if preset.is_some_and(|p| {
            p.spec.url == spec.url
                && p.spec.method == spec.method
                && p.spec.credential_header == spec.credential_header
                && p.spec.credential_prefix == spec.credential_prefix
                && p.spec.headers == spec.headers
        }) {
            return Ok(());
        }
        Err(HttpChecksError::Invalid { reason: "Write-only secrets can only be verified using their value-recognized provider's reviewed endpoint and authentication headers. Supply an explicit credential for custom endpoints.".into() })
    }
    pub async fn detect(
        &self,
        project_id: i32,
        env_var_id: i32,
    ) -> Result<DetectionView, HttpChecksError> {
        let (key, value, _) = self.env_credential(project_id, env_var_id).await?;
        Ok(DetectionView {
            env_var_id,
            candidates: self.detector.detect(&key, &value),
            detection_rule_count: self.detector.rule_count(),
        })
    }
    pub async fn save(
        &self,
        project_id: i32,
        id: Option<i32>,
        input: SaveHttpCheck,
    ) -> Result<HttpCheckView, HttpChecksError> {
        if input.name.trim().is_empty()
            || input.name.len() > 120
            || !(300..=604800).contains(&input.interval_seconds)
        {
            return Err(HttpChecksError::Invalid {reason:"Name must contain 1–120 characters; interval must be between 300 and 604800 seconds.".into()});
        }
        HttpCredentialVerifier::new(input.spec.clone()).map_err(|e| HttpChecksError::Invalid {
            reason: e.to_string(),
        })?;
        let url =
            temps_core::url_validation::validate_external_url(&input.spec.url).map_err(|_| {
                HttpChecksError::Invalid {
                    reason: "Endpoint must be a public HTTPS URL.".into(),
                }
            })?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(HttpChecksError::Invalid {
                reason: "Put credentials in headers, not the URL.".into(),
            });
        }
        if input.env_var_id.is_some() && input.credential.is_some() {
            return Err(HttpChecksError::Invalid {
                reason: "Choose an environment variable or an inline credential, not both.".into(),
            });
        }
        if let Some(env_id) = input.env_var_id {
            let (key, value, is_secret) = self.env_credential(project_id, env_id).await?;
            self.validate_credential_destination(&key, &value, is_secret, &input.spec)?;
        }
        let existing = match id {
            Some(id) => Some(self.row(project_id, id).await?),
            None => None,
        };
        if projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "find project", e))?
            .is_none()
        {
            return Err(HttpChecksError::Invalid {
                reason: "Project does not exist.".into(),
            });
        }
        if let Some(secret) = &input.credential {
            if secret.len() > 16_384 || secret.contains(['\r', '\n']) {
                return Err(HttpChecksError::Invalid {
                    reason: "Credential is too long or contains newlines.".into(),
                });
            }
        }
        let credential = match input.credential {
            Some(value) => Some(
                self.encryption
                    .encrypt_string(&value)
                    .map_err(|_| HttpChecksError::Encryption { project_id })?,
            ),
            None if input.env_var_id.is_some() => None,
            None => existing
                .as_ref()
                .and_then(|m| m.encrypted_credential.clone()),
        };
        if input.spec.credential_header.is_some()
            && input.env_var_id.is_none()
            && credential.is_none()
        {
            return Err(HttpChecksError::Invalid {
                reason: "This HTTP check requires a credential source.".into(),
            });
        }
        let spec = serde_json::to_string(&input.spec).map_err(|_| HttpChecksError::Invalid {
            reason: "HTTP recipe could not be encoded.".into(),
        })?;
        let encrypted_spec = self
            .encryption
            .encrypt_string(&spec)
            .map_err(|_| HttpChecksError::Encryption { project_id })?;
        let now = Utc::now();
        let mut model: http_checks::ActiveModel = existing.map(Into::into).unwrap_or_default();
        model.project_id = Set(project_id);
        model.automatic_provider = Set(None);
        model.env_var_id = Set(input.env_var_id);
        model.name = Set(input.name.trim().into());
        model.encrypted_spec = Set(encrypted_spec);
        model.encrypted_credential = Set(credential);
        model.enabled = Set(input.enabled);
        model.interval_seconds = Set(input.interval_seconds);
        model.next_check_at = Set(now);
        model.lease_until = Set(None);
        model.lease_token = Set(None);
        model.last_result = Set(None);
        model.last_checked_at = Set(None);
        if id.is_none() {
            model.last_notified_fingerprint = Set(String::new());
        }
        model.consecutive_unknowns = Set(0);
        model.updated_at = Set(now);
        let saved = if id.is_some() {
            model.update(self.db.as_ref()).await
        } else {
            model.created_at = Set(now);
            model.insert(self.db.as_ref()).await
        }
        .map_err(|e| db_error(project_id, "save", e))?;
        saved.try_into()
    }
    pub async fn delete(&self, project_id: i32, id: i32) -> Result<(), HttpChecksError> {
        let deleted = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH variable_lock AS MATERIALIZED ( \
                     SELECT id FROM env_vars WHERE id = (SELECT env_var_id FROM http_checks WHERE project_id = $1 AND id = $2) FOR UPDATE \
                 ), target AS ( \
                     SELECT checks.id, checks.env_var_id, checks.automatic_provider FROM http_checks AS checks \
                     WHERE checks.project_id = $1 AND checks.id = $2 \
                       AND (checks.env_var_id IS NULL OR EXISTS(SELECT 1 FROM variable_lock)) \
                     FOR UPDATE OF checks \
                ), suppression AS ( \
                     INSERT INTO env_check_suppressions(env_var_id, automatic_provider) \
                     SELECT env_var_id, automatic_provider FROM target \
                     WHERE env_var_id IS NOT NULL AND automatic_provider IS NOT NULL \
                     ON CONFLICT (env_var_id, automatic_provider) DO NOTHING \
                 ) \
                 DELETE FROM http_checks WHERE id IN (SELECT id FROM target) RETURNING id",
                [project_id.into(), id.into()],
            ))
            .await
            .map_err(|e| db_error(project_id, "delete", e))?;
        if deleted.is_none() {
            return Err(HttpChecksError::NotFound { project_id, id });
        }
        Ok(())
    }
    pub async fn run_now(
        &self,
        project_id: i32,
        id: i32,
    ) -> Result<HttpCheckView, HttpChecksError> {
        let _permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| HttpChecksError::Busy { id })?;
        self.row(project_id, id).await?;
        let token = uuid::Uuid::new_v4().to_string();
        let statement=Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE http_checks SET lease_until=NOW()+INTERVAL '60 seconds',lease_token=$3 WHERE id=$1 AND project_id=$2 AND (lease_until IS NULL OR lease_until<NOW()) AND (last_checked_at IS NULL OR last_checked_at<NOW()-INTERVAL '30 seconds') RETURNING *", [id.into(),project_id.into(),token.into()]);
        let row = http_checks::Entity::find()
            .from_raw_sql(statement)
            .one(self.db.as_ref())
            .await
            .map_err(|e| db_error(project_id, "claim", e))?
            .ok_or(HttpChecksError::Busy { id })?;
        self.execute(row).await?;
        self.row(project_id, id).await?.try_into()
    }
    async fn execute(&self, row: http_checks::Model) -> Result<(), HttpChecksError> {
        let now = Utc::now();
        let outcome = self.verify_row(&row, now).await;
        // Storage failures also become an honest unknown result; no error strings from
        // encryption, URLs or providers are persisted or sent in notifications.
        let result=outcome.unwrap_or_else(|error| {
            tracing::warn!(project_id=row.project_id,check_id=row.id,error=%error,"HTTP check could not be verified");
            VerificationResult::single(CheckStatus::Unknown,"check_unavailable","The check could not run. Review its configuration and credential source.",now)
        });
        let unknowns = if result.status == CheckStatus::Unknown {
            row.consecutive_unknowns.saturating_add(1)
        } else {
            0
        };
        let result_json =
            serde_json::to_value(&result).map_err(|_| HttpChecksError::Stored { id: row.id })?;
        let updated = http_checks::Entity::update_many()
            .col_expr(http_checks::Column::LastResult, Expr::value(result_json))
            .col_expr(http_checks::Column::LastCheckedAt, Expr::value(now))
            .col_expr(
                http_checks::Column::ConsecutiveUnknowns,
                Expr::value(unknowns),
            )
            .filter(http_checks::Column::Id.eq(row.id))
            .filter(http_checks::Column::LeaseToken.eq(row.lease_token.clone()))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| db_error(row.project_id, "record result", e))?;
        if updated.rows_affected == 0 {
            return Ok(());
        } // Configuration changed or row was deleted during the request.
        let fingerprint = result.fingerprint();
        let should_notify = fingerprint != row.last_notified_fingerprint
            && (result.status != CheckStatus::Unknown || unknowns >= 2);
        let mut notified = row.last_notified_fingerprint.clone();
        let mut retry = false;
        if should_notify && self.notifications.is_configured().await.unwrap_or(false) {
            let severity = match result.status {
                CheckStatus::Healthy => "recovered",
                CheckStatus::Error => "failed",
                CheckStatus::Warning => "warning",
                CheckStatus::Unknown => "inconclusive",
            };
            let message = result
                .findings
                .iter()
                .filter(|f| {
                    f.status != CheckStatus::Healthy || result.status == CheckStatus::Healthy
                })
                .map(|f| f.message.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let notification = NotificationData {
                title: format!("HTTP check '{}' {severity}", row.name),
                message,
                notification_type: if result.status == CheckStatus::Healthy {
                    NotificationType::Info
                } else {
                    NotificationType::Alert
                },
                priority: if result.status == CheckStatus::Error {
                    NotificationPriority::High
                } else {
                    NotificationPriority::Normal
                },
                metadata: [
                    ("project_id".into(), row.project_id.to_string()),
                    ("http_check_id".into(), row.id.to_string()),
                ]
                .into(),
                ..Default::default()
            };
            match tokio::time::timeout(
                std::time::Duration::from_secs(20),
                self.notifications.send_notification(notification),
            )
            .await
            {
                Ok(Ok(())) => notified = fingerprint,
                Ok(Err(_)) | Err(_) => {
                    retry = true;
                    tracing::warn!(
                        project_id = row.project_id,
                        check_id = row.id,
                        "HTTP check notification failed; retry scheduled"
                    );
                }
            }
        }
        let next = now
            + Duration::seconds(
                if retry || (result.status == CheckStatus::Unknown && unknowns < 2) {
                    300
                } else {
                    row.interval_seconds as i64
                },
            );
        http_checks::Entity::update_many()
            .col_expr(
                http_checks::Column::LastNotifiedFingerprint,
                Expr::value(notified),
            )
            .col_expr(http_checks::Column::NextCheckAt, Expr::value(next))
            .col_expr(
                http_checks::Column::LeaseUntil,
                Expr::value(None::<DateTime<Utc>>),
            )
            .col_expr(http_checks::Column::LeaseToken, Expr::value(None::<String>))
            .filter(http_checks::Column::Id.eq(row.id))
            .filter(http_checks::Column::LeaseToken.eq(row.lease_token))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| db_error(row.project_id, "finish check", e))?;
        Ok(())
    }
    async fn verify_row(
        &self,
        row: &http_checks::Model,
        now: DateTime<Utc>,
    ) -> Result<VerificationResult, HttpChecksError> {
        let serialized = self
            .encryption
            .decrypt_string(&row.encrypted_spec)
            .map_err(|_| HttpChecksError::Encryption {
                project_id: row.project_id,
            })?;
        let spec: HttpCheckSpec = serde_json::from_str(&serialized)
            .map_err(|_| HttpChecksError::Stored { id: row.id })?;
        let credential = if let Some(id) = row.env_var_id {
            let (key, value, is_secret) = self.env_credential(row.project_id, id).await?;
            self.validate_credential_destination(
                &key,
                &value,
                is_secret || row.automatic_provider.is_some(),
                &spec,
            )?;
            Some(value)
        } else {
            row.encrypted_credential
                .as_ref()
                .map(|s| self.encryption.decrypt_string(s))
                .transpose()
                .map_err(|_| HttpChecksError::Encryption {
                    project_id: row.project_id,
                })?
        };
        if row.env_var_id.is_some() {
            let current = self.row(row.project_id, row.id).await?;
            if current.lease_token != row.lease_token || row.lease_token.is_none() {
                return Err(HttpChecksError::Busy { id: row.id });
            }
        }
        let verifier = HttpCredentialVerifier::new(spec)
            .map_err(|_| HttpChecksError::Stored { id: row.id })?;
        Ok(match verifier.verify(credential.as_deref(),self.transport.as_ref(),now).await {
            Ok(result)=>result,
            Err(_)=>VerificationResult::single(CheckStatus::Unknown,"verification_inconclusive","The HTTP check could not complete. Check the endpoint, permissions, or connection.",now),
        })
    }
    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = self.reconcile_variables().await {
                    tracing::warn!(error=%error,"Automatic credential detection failed");
                }
                let Ok(permit) = self.permits.clone().try_acquire_owned() else {
                    continue;
                };
                // One indexed claim per tick, independent of catalog or total row count.
                // Atomic lease is safe across multiple Temps control-plane processes.
                let statement=Statement::from_sql_and_values(DatabaseBackend::Postgres,
                    "UPDATE http_checks SET lease_until=NOW()+INTERVAL '60 seconds',lease_token=$1 WHERE id=(SELECT id FROM http_checks WHERE enabled AND next_check_at<=NOW() AND (lease_until IS NULL OR lease_until<NOW()) ORDER BY next_check_at,id LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING *", [uuid::Uuid::new_v4().to_string().into()]);
                match http_checks::Entity::find()
                    .from_raw_sql(statement)
                    .one(self.db.as_ref())
                    .await
                {
                    Ok(Some(row)) => {
                        let service = self.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(e) = service.execute(row).await {
                                tracing::error!(error=%e,"Scheduled HTTP check failed");
                            }
                        });
                    }
                    Ok(None) => {}
                    Err(e) => tracing::error!(error=%e,"Could not claim a due HTTP check"),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{MockDatabase, MockExecResult};
    use temps_core::notifications::{EmailMessage, NotificationError};
    struct Notifications;
    #[async_trait]
    impl NotificationService for Notifications {
        async fn send_email(&self, _: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn send_notification(&self, _: NotificationData) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }
    fn service(db: MockDatabase) -> HttpChecksService {
        HttpChecksService::new(
            Arc::new(db.into_connection()),
            Arc::new(EncryptionService::new_from_password(
                "test-only-not-a-live-credential",
            )),
            Arc::new(Notifications),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn write_only_secrets_reject_custom_destinations_on_save_and_execution() {
        let env = env_vars::Model {
            id: 7,
            project_id: 10,
            environment_id: None,
            key: "GITHUB_TOKEN".into(),
            value: "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            include_in_preview: false,
            is_encrypted: false,
            is_secret: true,
        };
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![env.clone()], vec![env.clone()]]),
        );
        let mut spec = temps_credential_checks::provider_presets().remove(0).spec;
        assert!(s
            .validate_credential_destination(&env.key, &env.value, true, &spec)
            .is_ok());
        assert!(s
            .validate_credential_destination(&env.key, "unrelated-secret", true, &spec)
            .is_err());
        spec.url = "https://example.com/collect".into();
        assert!(s
            .validate_credential_destination(&env.key, &env.value, false, &spec)
            .is_ok());
        assert!(matches!(
            s.save(
                10,
                None,
                SaveHttpCheck {
                    name: "Custom".into(),
                    env_var_id: Some(7),
                    credential: None,
                    enabled: true,
                    interval_seconds: 86400,
                    spec: spec.clone()
                }
            )
            .await,
            Err(HttpChecksError::Invalid { .. })
        ));
        let mut record = row();
        record.env_var_id = Some(7);
        record.encrypted_spec = s
            .encryption
            .encrypt_string(&serde_json::to_string(&spec).unwrap())
            .unwrap();
        assert!(matches!(
            s.verify_row(&record, Utc::now()).await,
            Err(HttpChecksError::Invalid { .. })
        ));
    }

    fn row() -> http_checks::Model {
        let now = Utc::now();
        http_checks::Model {
            id: 1,
            project_id: 10,
            env_var_id: None,
            name: "Example endpoint".into(),
            automatic_provider: None,
            encrypted_spec: "ciphertext".into(),
            encrypted_credential: Some("ciphertext".into()),
            enabled: true,
            interval_seconds: 86400,
            next_check_at: now,
            lease_until: None,
            lease_token: None,
            last_result: None,
            last_checked_at: None,
            last_notified_fingerprint: String::new(),
            consecutive_unknowns: 0,
            created_at: now,
            updated_at: now,
        }
    }
    #[tokio::test]
    async fn missing_and_database_failures_remain_typed() {
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<http_checks::Model>::new()]),
        );
        assert!(matches!(
            s.row(10, 99).await,
            Err(HttpChecksError::NotFound {
                project_id: 10,
                id: 99
            })
        ));
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("test failure".into())]),
        );
        assert!(matches!(
            s.row(10, 1).await,
            Err(HttpChecksError::Database { project_id: 10, .. })
        ));
    }
    #[tokio::test]
    async fn delete_requires_an_existing_scoped_row() {
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<http_checks::Model>::new()]),
        );
        assert!(matches!(
            s.delete(10, 2).await,
            Err(HttpChecksError::NotFound { .. })
        ));
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![row()]]),
        );
        assert!(s.delete(10, 1).await.is_ok());
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("delete failed".into())]),
        );
        assert!(matches!(
            s.delete(10, 1).await,
            Err(HttpChecksError::Database {
                project_id: 10,
                operation: "delete",
                ..
            })
        ));
    }
    #[tokio::test]
    async fn save_rejects_invalid_input_before_database_or_network_access() {
        let s = service(MockDatabase::new(DatabaseBackend::Postgres));
        let spec = temps_credential_checks::provider_presets().remove(0).spec;
        for (name, interval) in [("", 86400), ("check", 10)] {
            assert!(matches!(
                s.save(
                    10,
                    None,
                    SaveHttpCheck {
                        name: name.into(),
                        env_var_id: None,
                        credential: None,
                        spec: spec.clone(),
                        interval_seconds: interval,
                        enabled: true
                    }
                )
                .await,
                Err(HttpChecksError::Invalid { .. })
            ));
        }
    }
    #[test]
    fn responses_never_serialize_secrets_and_corrupt_results_fail_closed() {
        let view = HttpCheckView::try_from(row()).unwrap();
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("ciphertext"));
        assert!(!json.contains("encrypted"));
        let mut record = row();
        record.last_result = Some(serde_json::json!({"unexpected":"value"}));
        assert!(matches!(
            HttpCheckView::try_from(record),
            Err(HttpChecksError::Stored { .. })
        ));
    }
    #[tokio::test]
    async fn detection_rejects_cross_project_or_missing_variables() {
        let s = service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<env_vars::Model>::new()]),
        );
        assert!(matches!(
            s.detect(10, 99).await,
            Err(HttpChecksError::Invalid { .. })
        ));
    }
    struct RecordingNotifications(std::sync::Mutex<Vec<NotificationData>>);
    #[async_trait]
    impl NotificationService for RecordingNotifications {
        async fn send_email(&self, _: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }
        async fn send_notification(
            &self,
            notification: NotificationData,
        ) -> Result<(), NotificationError> {
            self.0.lock().unwrap().push(notification);
            Ok(())
        }
        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(true)
        }
    }
    struct ResponseTransport(u16);
    #[async_trait]
    impl HttpCheckTransport for ResponseTransport {
        async fn execute(
            &self,
            _: temps_credential_checks::HttpCheckRequest,
        ) -> Result<
            temps_credential_checks::HttpCheckResponse,
            temps_credential_checks::VerificationError,
        > {
            Ok(temps_credential_checks::HttpCheckResponse {
                status: self.0,
                headers: Default::default(),
                body: b"{}".to_vec(),
            })
        }
    }
    #[tokio::test]
    async fn notifications_deduplicate_recover_and_debounce_unknown_results() {
        for (status, previous, unknowns, expected) in [
            (200, "", 0, 0),
            (401, "", 0, 1),
            (401, "authentication_rejected", 0, 0),
            (200, "authentication_rejected", 0, 1),
            (503, "", 0, 0),
            (503, "", 1, 1),
        ] {
            let mut service = service(
                MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                ]),
            );
            let notifications = Arc::new(RecordingNotifications(Default::default()));
            service.notifications = notifications.clone();
            service.transport = Arc::new(ResponseTransport(status));
            let mut record = row();
            let mut spec = temps_credential_checks::provider_presets().remove(0).spec;
            spec.credential_header = None;
            record.encrypted_spec = service
                .encryption
                .encrypt_string(&serde_json::to_string(&spec).unwrap())
                .unwrap();
            record.encrypted_credential = None;
            record.lease_token = Some("test-lease".into());
            record.last_notified_fingerprint = previous.into();
            record.consecutive_unknowns = unknowns;
            service.execute(record).await.unwrap();
            let sent = notifications.0.lock().unwrap();
            assert_eq!(
                sent.len(),
                expected,
                "status={status}, previous={previous}, unknowns={unknowns}"
            );
            if status == 200 && expected == 1 {
                assert!(sent[0].title.contains("recovered"));
            }
        }
    }
    #[tokio::test]
    async fn stale_worker_does_not_notify_after_credential_rotation() {
        let mut service = service(
            MockDatabase::new(DatabaseBackend::Postgres).append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }]),
        );
        let notifications = Arc::new(RecordingNotifications(Default::default()));
        service.notifications = notifications.clone();
        let mut record = row();
        record.consecutive_unknowns = 3;
        service.execute(record).await.unwrap();
        assert!(notifications.0.lock().unwrap().is_empty());
    }
}
