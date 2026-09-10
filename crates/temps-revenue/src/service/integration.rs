// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration service: manages per-project revenue integrations.
//!
//! Responsibilities:
//!   * Generate a high-entropy webhook path token (256 bits)
//!   * Encrypt the user-supplied signing secret at rest
//!   * Enforce the "at most one active integration per (project, provider)"
//!     invariant (matches the partial unique index)
//!   * Never leak the stored secret back to callers — only expose a
//!     `has_secret` boolean and the path token

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::{Deserialize, Serialize};
use temps_core::EncryptionService;
use temps_entities::revenue_integrations::{
    self, ActiveModel as IntegrationActiveModel, Entity as IntegrationEntity,
    Model as IntegrationModel,
};

use crate::error::RevenueError;
use crate::providers::{ProviderConfig, ProviderRegistry};

/// 32 bytes = 256 bits of entropy before base64url encoding.
const PATH_TOKEN_BYTES: usize = 32;

const STATUS_PENDING: &str = "pending";
const STATUS_ACTIVE: &str = "active";
const STATUS_DISABLED: &str = "disabled";

#[derive(Debug, Deserialize)]
pub struct CreateIntegrationInput {
    pub project_id: i32,
    pub provider: String,
    pub signing_secret: String,
}

/// API-safe projection of an integration. Never contains the secret.
#[derive(Debug, Clone, Serialize)]
pub struct IntegrationView {
    pub id: i32,
    pub project_id: i32,
    pub provider: String,
    pub webhook_path_token: String,
    pub status: String,
    pub has_secret: bool,
    pub last_event_at: Option<chrono::DateTime<Utc>>,
    pub created_at: chrono::DateTime<Utc>,
    /// Typed provider config (price/product allowlist, metered billing
    /// mode). `None` means no filtering — accept every event as-is. This
    /// is the safe default for brand new integrations.
    pub config: Option<ProviderConfig>,
}

impl From<&IntegrationModel> for IntegrationView {
    fn from(m: &IntegrationModel) -> Self {
        Self {
            id: m.id,
            project_id: m.project_id,
            provider: m.provider.clone(),
            webhook_path_token: m.webhook_path_token.clone(),
            status: m.status.clone(),
            has_secret: !m.webhook_signing_secret_encrypted.is_empty(),
            last_event_at: m.last_event_at,
            created_at: m.created_at,
            config: ProviderConfig::from_value(m.config.as_ref()),
        }
    }
}

pub struct RevenueIntegrationService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
    providers: ProviderRegistry,
}

impl RevenueIntegrationService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        providers: ProviderRegistry,
    ) -> Self {
        Self {
            db,
            encryption,
            providers,
        }
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    /// Creates a new pending integration and returns its API-safe view.
    pub async fn create(
        &self,
        input: CreateIntegrationInput,
    ) -> Result<IntegrationModel, RevenueError> {
        if input.signing_secret.trim().is_empty() {
            return Err(RevenueError::Validation {
                message: "signing_secret cannot be empty".into(),
            });
        }

        if self.providers.get(&input.provider).is_none() {
            return Err(RevenueError::UnknownProvider {
                provider: input.provider.clone(),
            });
        }

        // Enforce invariant at the service layer so we can return a
        // typed error before hitting the partial unique index.
        if let Some(existing) = self
            .find_active_integration(input.project_id, &input.provider)
            .await?
        {
            return Err(RevenueError::DuplicateIntegration {
                project_id: input.project_id,
                provider: input.provider,
                existing_integration_id: existing.id,
            });
        }

        let token = generate_path_token()?;
        let encrypted = self
            .encryption
            .encrypt_string(&input.signing_secret)
            .map_err(|e| RevenueError::EncryptionFailed {
                project_id: input.project_id,
                reason: e.to_string(),
            })?;

        let model = IntegrationActiveModel {
            project_id: Set(input.project_id),
            provider: Set(input.provider),
            webhook_path_token: Set(token),
            webhook_signing_secret_encrypted: Set(encrypted),
            status: Set(STATUS_PENDING.to_string()),
            last_event_at: Set(None),
            ..Default::default()
        };

        let inserted = model.insert(self.db.as_ref()).await?;
        Ok(inserted)
    }

    pub async fn list_for_project(
        &self,
        project_id: i32,
    ) -> Result<Vec<IntegrationModel>, RevenueError> {
        let rows = IntegrationEntity::find()
            .filter(revenue_integrations::Column::ProjectId.eq(project_id))
            .order_by_desc(revenue_integrations::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;
        Ok(rows)
    }

    pub async fn get(
        &self,
        project_id: i32,
        integration_id: i32,
    ) -> Result<IntegrationModel, RevenueError> {
        IntegrationEntity::find_by_id(integration_id)
            .filter(revenue_integrations::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(RevenueError::IntegrationNotFound {
                integration_id,
                project_id,
            })
    }

    /// Looks up an integration by its public path token. Used by the
    /// webhook ingestion handler — must stay O(1) via the unique index.
    pub async fn get_by_path_token(&self, token: &str) -> Result<IntegrationModel, RevenueError> {
        IntegrationEntity::find()
            .filter(revenue_integrations::Column::WebhookPathToken.eq(token))
            .one(self.db.as_ref())
            .await?
            .ok_or(RevenueError::IntegrationNotFoundByToken)
    }

    pub async fn decrypt_signing_secret(
        &self,
        integration: &IntegrationModel,
    ) -> Result<String, RevenueError> {
        self.encryption
            .decrypt_string(&integration.webhook_signing_secret_encrypted)
            .map_err(|e| RevenueError::DecryptionFailed {
                integration_id: integration.id,
                reason: e.to_string(),
            })
    }

    /// Flips a pending integration to active on the first successful
    /// event ingest. Idempotent for subsequent events.
    pub async fn mark_active(
        &self,
        integration_id: i32,
        event_at: chrono::DateTime<Utc>,
    ) -> Result<(), RevenueError> {
        let Some(row) = IntegrationEntity::find_by_id(integration_id)
            .one(self.db.as_ref())
            .await?
        else {
            return Err(RevenueError::IntegrationNotFoundByToken);
        };

        let mut active: IntegrationActiveModel = row.into();
        active.status = Set(STATUS_ACTIVE.to_string());
        active.last_event_at = Set(Some(event_at));
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    pub async fn disable(&self, project_id: i32, integration_id: i32) -> Result<(), RevenueError> {
        let row = self.get(project_id, integration_id).await?;
        let mut active: IntegrationActiveModel = row.into();
        active.status = Set(STATUS_DISABLED.to_string());
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    pub async fn delete(&self, project_id: i32, integration_id: i32) -> Result<(), RevenueError> {
        let row = self.get(project_id, integration_id).await?;
        let _ = IntegrationEntity::delete_by_id(row.id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }

    /// Replaces the stored signing secret with a new one. The path token
    /// is left untouched so the provider-side webhook URL does not need
    /// to change. Used when the user rotates their signing secret in the
    /// provider's dashboard.
    pub async fn update_signing_secret(
        &self,
        project_id: i32,
        integration_id: i32,
        new_secret: &str,
    ) -> Result<IntegrationModel, RevenueError> {
        if new_secret.trim().is_empty() {
            return Err(RevenueError::Validation {
                message: "signing_secret cannot be empty".into(),
            });
        }

        let row = self.get(project_id, integration_id).await?;
        let encrypted = self.encryption.encrypt_string(new_secret).map_err(|e| {
            RevenueError::EncryptionFailed {
                project_id,
                reason: e.to_string(),
            }
        })?;

        let mut active: IntegrationActiveModel = row.into();
        active.webhook_signing_secret_encrypted = Set(encrypted);
        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Replaces the typed provider config on an integration.
    ///
    /// Passing `None` clears the config back to the accept-everything
    /// default; passing `Some` validates that the provider tag in the
    /// config matches the integration's provider (so an operator can't
    /// accidentally attach a Stripe config to a LemonSqueezy integration).
    pub async fn update_config(
        &self,
        project_id: i32,
        integration_id: i32,
        config: Option<ProviderConfig>,
    ) -> Result<IntegrationModel, RevenueError> {
        let row = self.get(project_id, integration_id).await?;

        if let Some(ref cfg) = config {
            let tag = match cfg {
                ProviderConfig::Stripe(_) => "stripe",
                ProviderConfig::LemonSqueezy(_) => "lemon_squeezy",
            };
            if tag != row.provider {
                return Err(RevenueError::Validation {
                    message: format!(
                        "config provider '{}' does not match integration provider '{}'",
                        tag, row.provider
                    ),
                });
            }
        }

        let value = match config {
            Some(cfg) => {
                Some(
                    serde_json::to_value(&cfg).map_err(|e| RevenueError::Validation {
                        message: format!("failed to serialize provider config: {}", e),
                    })?,
                )
            }
            None => None,
        };

        let mut active: IntegrationActiveModel = row.into();
        active.config = Set(value);
        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Rotates the webhook path token. Users need to paste the new URL
    /// into their provider's dashboard after rotation.
    pub async fn rotate_path_token(
        &self,
        project_id: i32,
        integration_id: i32,
    ) -> Result<IntegrationModel, RevenueError> {
        let row = self.get(project_id, integration_id).await?;
        let mut active: IntegrationActiveModel = row.into();
        active.webhook_path_token = Set(generate_path_token()?);
        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    async fn find_active_integration(
        &self,
        project_id: i32,
        provider: &str,
    ) -> Result<Option<IntegrationModel>, RevenueError> {
        let row = IntegrationEntity::find()
            .filter(revenue_integrations::Column::ProjectId.eq(project_id))
            .filter(revenue_integrations::Column::Provider.eq(provider))
            .filter(revenue_integrations::Column::Status.ne(STATUS_DISABLED))
            .one(self.db.as_ref())
            .await?;
        Ok(row)
    }
}

fn generate_path_token() -> Result<String, RevenueError> {
    let mut bytes = [0u8; PATH_TOKEN_BYTES];
    fill_random_bytes_with(
        &mut rand::rngs::SysRng,
        "generating a revenue webhook path token",
        &mut bytes,
    )?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn fill_random_bytes_with<R: rand::TryCryptoRng>(
    rng: &mut R,
    operation: &str,
    bytes: &mut [u8],
) -> Result<(), RevenueError> {
    rng.try_fill_bytes(bytes)
        .map_err(|error| RevenueError::RandomnessFailed {
            operation: operation.to_string(),
            reason: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::revenue_integrations;

    struct FailingCryptoRng;

    impl rand::TryRng for FailingCryptoRng {
        type Error = std::io::Error;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Err(std::io::Error::other("revenue entropy unavailable"))
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            Err(std::io::Error::other("revenue entropy unavailable"))
        }

        fn try_fill_bytes(&mut self, _dst: &mut [u8]) -> Result<(), Self::Error> {
            Err(std::io::Error::other("revenue entropy unavailable"))
        }
    }

    impl rand::TryCryptoRng for FailingCryptoRng {}

    #[test]
    fn randomness_failure_preserves_revenue_operation_context() {
        let error = fill_random_bytes_with(
            &mut FailingCryptoRng,
            "testing revenue entropy",
            &mut [0u8; PATH_TOKEN_BYTES],
        )
        .unwrap_err();

        assert!(matches!(
            error,
            RevenueError::RandomnessFailed { operation, reason }
                if operation == "testing revenue entropy"
                    && reason.contains("revenue entropy unavailable")
        ));
    }

    fn make_encryption() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    fn sample_model(id: i32, project_id: i32, provider: &str, status: &str) -> IntegrationModel {
        revenue_integrations::Model {
            id,
            project_id,
            provider: provider.to_string(),
            webhook_path_token: "abcdefghij".to_string(),
            webhook_signing_secret_encrypted: "encrypted:bytes".to_string(),
            status: status.to_string(),
            last_event_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            config: None,
        }
    }

    #[test]
    fn path_token_has_expected_entropy() {
        let t1 = generate_path_token().unwrap();
        let t2 = generate_path_token().unwrap();
        assert_ne!(t1, t2);
        // base64url of 32 bytes, no padding = 43 chars
        assert_eq!(t1.len(), 43);
        assert!(t1
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn integration_view_never_contains_secret() {
        let model = sample_model(1, 42, "stripe", "pending");
        let view = IntegrationView::from(&model);
        assert_eq!(view.id, 1);
        assert_eq!(view.project_id, 42);
        assert!(view.has_secret, "should detect non-empty ciphertext");
        // has_secret is a bool — never the plaintext.
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains("encrypted"),
            "encrypted ciphertext leaked into JSON"
        );
    }

    #[tokio::test]
    async fn create_rejects_empty_secret() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc
            .create(CreateIntegrationInput {
                project_id: 1,
                provider: "stripe".into(),
                signing_secret: "   ".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, RevenueError::Validation { .. }));
    }

    #[tokio::test]
    async fn create_rejects_unknown_provider() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc
            .create(CreateIntegrationInput {
                project_id: 1,
                provider: "lemonsqueezy".into(),
                signing_secret: "whsec_something".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, RevenueError::UnknownProvider { .. }));
    }

    #[tokio::test]
    async fn create_rejects_duplicate_active_integration() {
        // Mock returns an existing active integration for the
        // (project_id, provider) pair — the service must return
        // DuplicateIntegration without even attempting an insert.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![sample_model(7, 42, "stripe", "active")]])
                .into_connection(),
        );
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc
            .create(CreateIntegrationInput {
                project_id: 42,
                provider: "stripe".into(),
                signing_secret: "whsec_something".into(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                RevenueError::DuplicateIntegration {
                    project_id: 42,
                    existing_integration_id: 7,
                    ..
                }
            ),
            "got wrong variant: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn get_scoped_to_project() {
        // Mock finds nothing (project_id filter would have filtered it
        // out) — service must return IntegrationNotFound with the
        // requested IDs preserved.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<revenue_integrations::Model>::new()])
                .into_connection(),
        );
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc.get(42, 99).await.unwrap_err();
        assert!(
            matches!(
                err,
                RevenueError::IntegrationNotFound {
                    integration_id: 99,
                    project_id: 42
                }
            ),
            "expected tenant-scoped NotFound, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn get_by_path_token_404_when_missing() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<revenue_integrations::Model>::new()])
                .into_connection(),
        );
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc.get_by_path_token("deadbeef").await.unwrap_err();
        assert!(matches!(err, RevenueError::IntegrationNotFoundByToken));
    }

    #[tokio::test]
    async fn update_signing_secret_rejects_empty() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc.update_signing_secret(42, 1, "   ").await.unwrap_err();
        assert!(matches!(err, RevenueError::Validation { .. }));
    }

    #[tokio::test]
    async fn update_signing_secret_scoped_to_project() {
        // Row lookup must apply project filter. Mock returns an empty
        // page so `get` surfaces IntegrationNotFound before the update
        // path ever runs.
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<revenue_integrations::Model>::new()])
                .into_connection(),
        );
        let svc = RevenueIntegrationService::new(
            db,
            make_encryption(),
            ProviderRegistry::default_registry(),
        );
        let err = svc
            .update_signing_secret(42, 99, "whsec_new")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RevenueError::IntegrationNotFound {
                integration_id: 99,
                project_id: 42
            }
        ));
    }

    #[tokio::test]
    async fn decrypt_signing_secret_roundtrips_encrypted_value() {
        let enc = make_encryption();
        let ciphertext = enc.encrypt_string("whsec_live_example").unwrap();
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = RevenueIntegrationService::new(db, enc, ProviderRegistry::default_registry());

        let mut model = sample_model(1, 42, "stripe", "active");
        model.webhook_signing_secret_encrypted = ciphertext;

        let plain = svc.decrypt_signing_secret(&model).await.unwrap();
        assert_eq!(plain, "whsec_live_example");
    }
}
