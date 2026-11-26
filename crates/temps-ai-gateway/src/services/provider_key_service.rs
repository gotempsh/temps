// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::ai_provider_keys;
use temps_entities::ai_provider_models;

use crate::error::AiGatewayError;
use crate::provider_url::{validate_provider_url, KeyTrust};
use crate::self_hosted::{is_self_hosted, SelfHostedCapabilities};

/// Who asked to trust a provider key's private address. Only the trust
/// itself is derived by the service -- from the key's own base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustRequest {
    pub set_by: Option<i32>,
}

/// Change to a key's per-key private-address trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustChange {
    /// Trust the exact `ip:port` of the key's (new) base URL.
    Set(TrustRequest),
    /// Remove the per-key trust.
    Clear,
}

/// Input for [`ProviderKeyService::create`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NewProviderKey<'a> {
    pub provider: &'a str,
    pub display_name: &'a str,
    /// Empty only for self-hosted providers without authentication.
    pub api_key: &'a str,
    pub base_url: Option<&'a str>,
    pub default_model: Option<&'a str>,
    /// Trust the exact private `ip:port` of `base_url` for this key only.
    pub trust: Option<TrustRequest>,
    /// Declared capabilities; self-hosted providers only.
    pub capabilities: Option<SelfHostedCapabilities>,
}

/// Input for [`ProviderKeyService::update`]. `None` leaves a field unchanged;
/// for the double options, `Some(None)` clears it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderKeyUpdate<'a> {
    pub display_name: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub base_url: Option<Option<&'a str>>,
    pub default_model: Option<Option<&'a str>>,
    pub is_active: Option<bool>,
    pub trust: Option<TrustChange>,
    pub capabilities: Option<SelfHostedCapabilities>,
}

/// Whether a model is listed in a key's catalog, or the key predates the
/// catalog and has none yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogMembership {
    Listed,
    LegacyUncataloged,
}

pub struct ProviderKeyService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
}

/// What a replacement credential is verified on, see
/// [`ProviderKeyService::replacement_check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementCheck {
    /// The model to verify on; `None` when nothing names one (a self-hosted
    /// key with no default and no catalog), which verification reports.
    pub model: Option<String>,
    /// Routing chose `model`, not a default the administrator set.
    pub chosen_by_routing: bool,
}

impl ProviderKeyService {
    pub fn new(db: Arc<DatabaseConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    pub async fn create(
        &self,
        new_key: NewProviderKey<'_>,
    ) -> Result<ai_provider_keys::Model, AiGatewayError> {
        let NewProviderKey {
            provider,
            display_name,
            api_key,
            base_url,
            default_model,
            trust,
            capabilities,
        } = new_key;
        let self_hosted = is_self_hosted(provider);
        if provider.is_empty() {
            return Err(AiGatewayError::Validation {
                message: "Provider cannot be empty".to_string(),
            });
        }
        // Self-hosted servers commonly run without authentication.
        if api_key.is_empty() && !self_hosted {
            return Err(AiGatewayError::Validation {
                message: format!("API key cannot be empty for provider '{provider}'"),
            });
        }
        let base_url = base_url.map(str::trim).filter(|url| !url.is_empty());
        if self_hosted && base_url.is_none() {
            return Err(AiGatewayError::Validation {
                message: "A base URL is required for an OpenAI-compatible provider, \
                          e.g. https://api.deepseek.com/v1 or http://10.0.0.5:8000/v1"
                    .to_string(),
            });
        }
        let key_trust = match (trust, base_url) {
            (Some(_), None) => {
                return Err(AiGatewayError::Validation {
                    message: "Trusting a private address requires a base URL".to_string(),
                })
            }
            (Some(_), Some(url)) => Some(KeyTrust::for_base_url(url)?),
            (None, _) => None,
        };
        // Store the canonical form of what was validated, so a lenient
        // spelling (`http:host`, stray whitespace) cannot be read differently
        // later.
        let base_url = match base_url {
            Some(url) => Some(validate_provider_url(url, key_trust).await?.to_string()),
            None => None,
        };
        if capabilities.is_some() && !self_hosted {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Capabilities can only be declared for OpenAI-compatible providers, not '{provider}'"
                ),
            });
        }

        let encrypted_key = self
            .encryption_service
            .encrypt_string(api_key)
            .map_err(|e| AiGatewayError::Encryption(e.to_string()))?;

        let now = chrono::Utc::now();
        let model = ai_provider_keys::ActiveModel {
            provider: Set(provider.to_string()),
            display_name: Set(display_name.to_string()),
            api_key_encrypted: Set(encrypted_key),
            base_url: Set(base_url),
            default_model: Set(default_model.map(|s| s.to_string())),
            is_active: Set(true),
            trusted_private_address: Set(key_trust.map(KeyTrust::to_stored)),
            trusted_private_address_set_by: Set(trust.and_then(|trust| trust.set_by)),
            trusted_private_address_set_at: Set(key_trust.map(|_| now)),
            capabilities: Set(self_hosted.then(|| capabilities.unwrap_or_default().to_stored())),
            ..Default::default()
        };

        let result = model.insert(self.db.as_ref()).await?;

        Ok(result)
    }

    pub async fn list(&self) -> Result<Vec<ai_provider_keys::Model>, AiGatewayError> {
        let keys = ai_provider_keys::Entity::find()
            .order_by_asc(ai_provider_keys::Column::Provider)
            .all(self.db.as_ref())
            .await?;

        Ok(keys)
    }

    pub async fn list_active(&self) -> Result<Vec<ai_provider_keys::Model>, AiGatewayError> {
        let keys = ai_provider_keys::Entity::find()
            .filter(ai_provider_keys::Column::IsActive.eq(true))
            .order_by_asc(ai_provider_keys::Column::Provider)
            .all(self.db.as_ref())
            .await?;

        Ok(keys)
    }

    /// The highest provider key ID right now, active or not; `None` with no
    /// keys. A background pass reads it once and stops there, so keys
    /// created while it runs wait for the next pass instead of extending
    /// this one without end.
    pub async fn last_key_id(&self) -> Result<Option<i32>, AiGatewayError> {
        let last = ai_provider_keys::Entity::find()
            .select_only()
            .column(ai_provider_keys::Column::Id)
            .order_by_desc(ai_provider_keys::Column::Id)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?;
        Ok(last)
    }

    /// Up to `limit` active key IDs greater than `after_id` and at most
    /// `through_id`, ascending: a background pass pages through them by ID
    /// instead of holding every key's row (credentials included) for its
    /// whole run, and ends at the [`Self::last_key_id`] it started with.
    pub async fn active_key_ids_after(
        &self,
        after_id: i32,
        through_id: i32,
        limit: u64,
    ) -> Result<Vec<i32>, AiGatewayError> {
        let ids = ai_provider_keys::Entity::find()
            .select_only()
            .column(ai_provider_keys::Column::Id)
            .filter(ai_provider_keys::Column::IsActive.eq(true))
            .filter(ai_provider_keys::Column::Id.gt(after_id))
            .filter(ai_provider_keys::Column::Id.lte(through_id))
            .order_by_asc(ai_provider_keys::Column::Id)
            .limit(limit)
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await?;
        Ok(ids)
    }

    pub async fn get_by_id(&self, id: i32) -> Result<ai_provider_keys::Model, AiGatewayError> {
        ai_provider_keys::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(AiGatewayError::ProviderKeyNotFound { key_id: id })
    }

    pub async fn get_active_by_provider(
        &self,
        provider: &str,
    ) -> Result<Option<ai_provider_keys::Model>, AiGatewayError> {
        let key = ai_provider_keys::Entity::find()
            .filter(ai_provider_keys::Column::Provider.eq(provider))
            .filter(ai_provider_keys::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?;

        Ok(key)
    }

    /// Enforce the operator-managed catalog for a system key. Legacy keys
    /// created before the catalog migration remain usable until their first
    /// bootstrap/refresh; once any catalog row exists, only enabled and
    /// currently available models may execute.
    pub async fn ensure_model_selectable(
        &self,
        key_id: i32,
        model_id: &str,
    ) -> Result<CatalogMembership, AiGatewayError> {
        if let Some(model) = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(key_id))
            .filter(ai_provider_models::Column::ModelId.eq(model_id))
            .one(self.db.as_ref())
            .await?
        {
            if model.is_enabled && model.is_available {
                return Ok(CatalogMembership::Listed);
            }
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Model '{model_id}' is disabled or unavailable for provider key {key_id}"
                ),
            });
        }

        let catalog_count = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(key_id))
            .count(self.db.as_ref())
            .await?;
        if catalog_count > 0 {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Model '{model_id}' is not in the enabled catalog for provider key {key_id}"
                ),
            });
        }
        Ok(CatalogMembership::LegacyUncataloged)
    }

    /// Every catalog row listing `model_id` (ignoring case), in any state, with its key --
    /// one JOIN query. Request routing decides from these which key may serve
    /// the model, so disabled rows and inactive keys are included on purpose:
    /// a model a self-hosted key hosts must not fall through to the name
    /// heuristic just because it is currently disabled there.
    pub async fn catalog_entries_for_model(
        &self,
        model_id: &str,
    ) -> Result<Vec<(ai_provider_models::Model, ai_provider_keys::Model)>, AiGatewayError> {
        // Matched ignoring case (served by `idx_ai_provider_models_model_id_lower`)
        // so a self-hosted listing that differs only in capitalisation still
        // blocks the name heuristic; serving requires the exact ID.
        let rows = ai_provider_models::Entity::find()
            .filter(
                sea_orm::sea_query::Expr::expr(sea_orm::sea_query::Func::lower(
                    sea_orm::sea_query::Expr::col((
                        ai_provider_models::Entity,
                        ai_provider_models::Column::ModelId,
                    )),
                ))
                .eq(model_id.to_lowercase()),
            )
            .find_also_related(ai_provider_keys::Entity)
            .order_by_asc(ai_provider_models::Column::ProviderKeyId)
            .all(self.db.as_ref())
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(model, key)| key.map(|key| (model, key)))
            .collect())
    }

    pub(crate) fn encrypt_api_key(&self, key: &str) -> Result<String, AiGatewayError> {
        self.encryption_service
            .encrypt_string(key)
            .map_err(|e| AiGatewayError::Encryption(e.to_string()))
    }

    /// Decrypt the API key from the stored encrypted value
    pub fn decrypt_api_key(&self, encrypted: &str) -> Result<String, AiGatewayError> {
        self.encryption_service
            .decrypt_string(encrypted)
            .map_err(|e| AiGatewayError::Encryption(e.to_string()))
    }

    pub async fn update(
        &self,
        id: i32,
        changes: ProviderKeyUpdate<'_>,
    ) -> Result<ai_provider_keys::Model, AiGatewayError> {
        let existing = self.get_by_id(id).await?;
        let self_hosted = is_self_hosted(&existing.provider);

        let base_url: Option<String> = match changes.base_url {
            Some(url) => url
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_string),
            None => existing.base_url.clone(),
        };
        if self_hosted && base_url.is_none() {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Self-hosted provider key {id} requires a base URL; it cannot be cleared"
                ),
            });
        }
        if changes.api_key == Some("") && !self_hosted {
            return Err(AiGatewayError::Validation {
                message: format!("API key cannot be empty for provider key {id}"),
            });
        }
        if changes.capabilities.is_some() && !self_hosted {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Capabilities can only be declared for OpenAI-compatible providers; key {id} is '{}'",
                    existing.provider
                ),
            });
        }

        let existing_trust = KeyTrust::from_stored(existing.trusted_private_address.as_deref());
        let new_trust = match changes.trust {
            Some(TrustChange::Set(_)) => {
                let url = base_url
                    .as_deref()
                    .ok_or_else(|| AiGatewayError::Validation {
                        message: format!(
                            "Trusting a private address requires a base URL on provider key {id}"
                        ),
                    })?;
                Some(KeyTrust::for_base_url(url)?)
            }
            Some(TrustChange::Clear) => None,
            // Trust is bound to the exact address in the base URL: moving
            // the base URL elsewhere drops it rather than carrying it over.
            None => existing_trust.filter(|trust| {
                base_url
                    .as_deref()
                    .and_then(|url| KeyTrust::for_base_url(url).ok())
                    == Some(*trust)
            }),
        };

        let mut base_url_changed = base_url != existing.base_url;
        let trust_changed = new_trust != existing_trust;
        let mut base_url = base_url;
        if base_url_changed || (trust_changed && new_trust.is_some()) {
            if let Some(url) = base_url.as_deref() {
                let canonical = validate_provider_url(url, new_trust).await?.to_string();
                base_url = Some(canonical);
                base_url_changed = base_url != existing.base_url;
            }
        }

        let mut active: ai_provider_keys::ActiveModel = existing.into();

        if let Some(name) = changes.display_name {
            active.display_name = Set(name.to_string());
        }

        if let Some(key) = changes.api_key {
            let encrypted = self
                .encryption_service
                .encrypt_string(key)
                .map_err(|e| AiGatewayError::Encryption(e.to_string()))?;
            active.api_key_encrypted = Set(encrypted);
        }

        if base_url_changed {
            active.base_url = Set(base_url);
        }

        if let Some(model) = changes.default_model {
            active.default_model = Set(model.map(|s| s.to_string()));
        }

        if let Some(active_flag) = changes.is_active {
            active.is_active = Set(active_flag);
        }

        if trust_changed || matches!(changes.trust, Some(TrustChange::Set(_))) {
            active.trusted_private_address = Set(new_trust.map(KeyTrust::to_stored));
            match (new_trust, changes.trust) {
                (Some(_), Some(TrustChange::Set(set_by))) => {
                    active.trusted_private_address_set_by = Set(set_by.set_by);
                    active.trusted_private_address_set_at = Set(Some(chrono::Utc::now()));
                }
                _ => {
                    active.trusted_private_address_set_by = Set(None);
                    active.trusted_private_address_set_at = Set(None);
                }
            }
        }

        if let Some(capabilities) = changes.capabilities {
            active.capabilities = Set(Some(capabilities.to_stored()));
        }

        let result = active.update(self.db.as_ref()).await?;

        Ok(result)
    }

    /// The model a replacement credential for `key` is verified on: the one
    /// internal calls will go to through the key after the update. That is
    /// the default model requested in the same update, else what routing
    /// picks for the key with its resulting pin (the pin when its catalog
    /// serves it, else the catalog's default): an unpinned key whose catalog
    /// enables only `gpt-5-nano` is verified on `gpt-5-nano`, not on the
    /// provider's newest low-cost model. `requested_default` is the update's
    /// `default_model` field: absent, cleared, or set.
    pub async fn replacement_check(
        &self,
        key: &ai_provider_keys::Model,
        requested_default: Option<Option<&str>>,
    ) -> Result<ReplacementCheck, AiGatewayError> {
        let pin = match requested_default {
            Some(Some(model)) => {
                return Ok(ReplacementCheck {
                    model: Some(model.to_string()),
                    chosen_by_routing: false,
                })
            }
            Some(None) => None,
            None => key.default_model.as_deref(),
        };
        let model = crate::services::ai_service::key_routed_model(
            self.db.as_ref(),
            key.id,
            &key.provider,
            pin,
        )
        .await?;
        let chosen_by_routing = model.as_deref() != pin.map(str::trim);
        Ok(ReplacementCheck {
            model,
            chosen_by_routing,
        })
    }

    /// The error for a replacement credential that failed verification on
    /// `check`'s model. The current key is kept either way; when routing
    /// (not the administrator) chose that model, the message says so and
    /// how to pick one the new credential can use.
    pub fn replacement_verification_failed(
        key: &ai_provider_keys::Model,
        check: &ReplacementCheck,
        reason: &str,
    ) -> AiGatewayError {
        let message = match (check.model.as_deref(), check.chosen_by_routing) {
            (Some(model), true) => format!(
                "The replacement API key for provider key {} ('{}') could not be verified on \
                 '{model}', the model this key sends internal requests to; the current key was \
                 kept: {reason}. If the new key cannot use '{model}', set the default model to \
                 one it can use in the same update.",
                key.id, key.display_name
            ),
            _ => format!(
                "API key verification failed for provider key {} ('{}'); the current key was \
                 kept: {reason}",
                key.id, key.display_name
            ),
        };
        AiGatewayError::Validation { message }
    }

    pub async fn delete(&self, id: i32) -> Result<(), AiGatewayError> {
        let _ = self.get_by_id(id).await?;
        if temps_entities::ai_gateway_objects::Entity::find()
            .filter(temps_entities::ai_gateway_objects::Column::ProviderKeyId.eq(id))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(AiGatewayError::Validation {
                message: "This key still owns gateway files or batches. Keep it configured to retrieve results, cancel jobs, and reconcile usage.".into(),
            });
        }
        ai_provider_keys::Entity::delete_by_id(id)
            .exec(self.db.as_ref())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn catalog_row(model_id: &str) -> temps_entities::ai_provider_models::Model {
        let now = chrono::Utc::now();
        temps_entities::ai_provider_models::Model {
            id: 1,
            provider_key_id: 1,
            model_id: model_id.to_string(),
            display_name: model_id.to_string(),
            source: "discovered".to_string(),
            is_available: true,
            is_enabled: true,
            owned_by: None,
            metadata: None,
            last_seen_at: Some(now),
            created_at: now,
            updated_at: now,
        }
    }

    fn check_service(db: MockDatabase) -> ProviderKeyService {
        ProviderKeyService::new(Arc::new(db.into_connection()), test_encryption())
    }

    #[tokio::test]
    async fn an_unpinned_replacement_is_verified_on_the_model_its_catalog_routes_to() {
        // Only gpt-5-nano is enabled and available: internal calls use it,
        // so the replacement is verified on it, not on the provider's newest
        // low-cost model.
        let service = check_service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![catalog_row("gpt-5-nano")]]),
        );
        let check = service
            .replacement_check(&sample_key(), None)
            .await
            .expect("check target");
        assert_eq!(
            check,
            ReplacementCheck {
                model: Some("gpt-5-nano".to_string()),
                chosen_by_routing: true,
            }
        );
    }

    #[tokio::test]
    async fn a_replacement_is_verified_on_the_pin_its_catalog_serves() {
        let mut key = sample_key();
        key.default_model = Some("gpt-5-mini".to_string());
        // The catalog is not empty and serves the pin.
        let service = check_service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
                vec![catalog_row("gpt-5-nano")],
                vec![catalog_row("gpt-5-mini")],
            ]),
        );
        let check = service.replacement_check(&key, None).await.expect("check");
        assert_eq!(check.model.as_deref(), Some("gpt-5-mini"));
        assert!(!check.chosen_by_routing);
    }

    #[tokio::test]
    async fn a_pin_the_catalog_no_longer_serves_is_not_what_is_verified() {
        let mut key = sample_key();
        key.default_model = Some("gpt-4o".to_string());
        // Not empty, the pin is not enabled, the catalog routes to gpt-5-nano.
        let service = check_service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([
                vec![catalog_row("gpt-5-nano")],
                vec![],
                vec![catalog_row("gpt-5-nano")],
            ]),
        );
        let check = service.replacement_check(&key, None).await.expect("check");
        assert_eq!(check.model.as_deref(), Some("gpt-5-nano"));
        assert!(check.chosen_by_routing);
    }

    #[tokio::test]
    async fn a_default_set_in_the_same_update_is_verified_without_a_lookup() {
        // No query results: a lookup would fail the test.
        let service = check_service(MockDatabase::new(DatabaseBackend::Postgres));
        let check = service
            .replacement_check(&sample_key(), Some(Some("gpt-5-mini")))
            .await
            .expect("check");
        assert_eq!(
            check,
            ReplacementCheck {
                model: Some("gpt-5-mini".to_string()),
                chosen_by_routing: false,
            }
        );
    }

    #[tokio::test]
    async fn a_never_refreshed_catalog_routes_to_the_providers_low_cost_default() {
        let service = check_service(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::ai_provider_models::Model>::new()]),
        );
        // Clearing the pin in the same update routes like an unpinned key.
        let mut key = sample_key();
        key.default_model = Some("gpt-5-mini".to_string());
        let check = service
            .replacement_check(&key, Some(None))
            .await
            .expect("check");
        assert_eq!(
            check.model.as_deref(),
            crate::providers::low_cost_default_model("openai")
        );
        assert!(check.chosen_by_routing);
    }

    #[tokio::test]
    async fn a_catalog_read_failure_is_an_error_not_a_guess() {
        let service = check_service(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_errors([
                sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
                    "connection reset while reading the catalog of key 1".to_string(),
                )),
            ]),
        );
        assert!(matches!(
            service.replacement_check(&sample_key(), None).await,
            Err(AiGatewayError::Database(_))
        ));
    }

    #[test]
    fn a_failed_replacement_names_the_routed_model_and_the_fix() {
        let key = sample_key();
        let routed = ReplacementCheck {
            model: Some("gpt-5-nano".to_string()),
            chosen_by_routing: true,
        };
        let AiGatewayError::Validation { message } =
            ProviderKeyService::replacement_verification_failed(&key, &routed, "model not found")
        else {
            panic!("a validation error");
        };
        assert!(
            message.contains("provider key 1 ('OpenAI Production')"),
            "{message}"
        );
        assert!(message.contains("verified on 'gpt-5-nano'"), "{message}");
        assert!(
            message.contains("current key was kept: model not found"),
            "{message}"
        );
        assert!(message.contains("set the default model"), "{message}");

        let chosen = ReplacementCheck {
            chosen_by_routing: false,
            ..routed
        };
        let AiGatewayError::Validation { message } =
            ProviderKeyService::replacement_verification_failed(&key, &chosen, "unauthorized")
        else {
            panic!("a validation error");
        };
        assert!(
            message.starts_with("API key verification failed for provider key 1"),
            "{message}"
        );
        assert!(!message.contains("set the default model"), "{message}");
    }

    /// A failed page must be an error, never an empty page: discovery reads an
    /// empty page as "every key seen" and records the pass as complete.
    #[tokio::test]
    async fn a_failed_active_key_page_is_an_error_not_an_empty_page() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(
                    "connection reset while reading page after key 911".to_string(),
                ))])
                .into_connection(),
        );
        let service = ProviderKeyService::new(db, test_encryption());
        match service.active_key_ids_after(911, i32::MAX, 100).await {
            Err(AiGatewayError::Database(error)) => {
                assert!(error.to_string().contains("after key 911"), "{error}");
            }
            other => panic!("expected a database error, got {other:?}"),
        }
    }

    fn test_encryption() -> Arc<EncryptionService> {
        Arc::new(EncryptionService::new("01234567890123456789012345678901").unwrap())
    }

    fn sample_key() -> ai_provider_keys::Model {
        ai_provider_keys::Model {
            id: 1,
            provider: "openai".to_string(),
            display_name: "OpenAI Production".to_string(),
            api_key_encrypted: "encrypted_value".to_string(),
            base_url: None,
            default_model: None,
            is_active: true,
            trusted_private_address: None,
            trusted_private_address_set_by: None,
            trusted_private_address_set_at: None,
            capabilities: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_list_active_keys() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![sample_key()]])
            .into_connection();

        let service = ProviderKeyService::new(Arc::new(db), test_encryption());
        let keys = service.list_active().await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].provider, "openai");
    }

    #[tokio::test]
    async fn test_get_by_id_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_provider_keys::Model>::new()])
            .into_connection();

        let service = ProviderKeyService::new(Arc::new(db), test_encryption());
        let result = service.get_by_id(999).await;
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::ProviderKeyNotFound { key_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_create_validates_empty_provider() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderKeyService::new(Arc::new(db), test_encryption());

        let result = service
            .create(NewProviderKey {
                provider: "",
                display_name: "Test",
                api_key: "sk-123",
                ..Default::default()
            })
            .await;
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_create_validates_empty_api_key() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderKeyService::new(Arc::new(db), test_encryption());

        let result = service
            .create(NewProviderKey {
                provider: "openai",
                display_name: "Test",
                api_key: "",
                ..Default::default()
            })
            .await;
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn self_hosted_key_requires_a_base_url_but_not_an_api_key() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderKeyService::new(Arc::new(db), test_encryption());
        let result = service
            .create(NewProviderKey {
                provider: crate::self_hosted::OPENAI_COMPATIBLE,
                display_name: "MLX",
                api_key: "",
                ..Default::default()
            })
            .await;
        match result {
            Err(AiGatewayError::Validation { message }) => {
                assert!(message.contains("base URL is required"), "{message}")
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn keyless_self_hosted_key_with_per_key_trust_is_stored() {
        let mut stored = sample_key();
        stored.provider = crate::self_hosted::OPENAI_COMPATIBLE.to_string();
        stored.base_url = Some("http://10.20.30.40:52415/v1".to_string());
        stored.trusted_private_address = Some("10.20.30.40:52415".to_string());
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![stored]])
                .into_connection(),
        );
        let service = ProviderKeyService::new(db.clone(), test_encryption());
        let key = service
            .create(NewProviderKey {
                provider: crate::self_hosted::OPENAI_COMPATIBLE,
                display_name: "MLX",
                api_key: "",
                base_url: Some("http://10.20.30.40:52415/v1"),
                trust: Some(TrustRequest { set_by: Some(1) }),
                ..Default::default()
            })
            .await
            .expect("private address trusted on the key");
        assert_eq!(
            key.trusted_private_address.as_deref(),
            Some("10.20.30.40:52415")
        );
        drop(service);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(sql.contains("10.20.30.40:52415"), "{sql}");
        // Default capabilities were stored for the self-hosted key.
        assert!(sql.contains("responses_api"), "{sql}");
    }

    #[tokio::test]
    async fn create_refuses_untrusted_private_and_plaintext_public_urls_before_storing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderKeyService::new(Arc::new(db), test_encryption());
        for (base_url, trust, expected) in [
            (
                "http://10.20.30.40:52415/v1",
                None,
                "Trusted private networks",
            ),
            ("http://8.8.8.8/v1", None, "plain http://"),
            (
                "http://models.internal:8000/v1",
                Some(TrustRequest { set_by: None }),
                "IP address",
            ),
            (
                "http://169.254.169.254/v1",
                Some(TrustRequest { set_by: None }),
                "metadata",
            ),
        ] {
            let result = service
                .create(NewProviderKey {
                    provider: crate::self_hosted::OPENAI_COMPATIBLE,
                    display_name: "MLX",
                    api_key: "secret-key",
                    base_url: Some(base_url),
                    trust,
                    ..Default::default()
                })
                .await;
            match result {
                Err(AiGatewayError::InvalidProviderUrl { reason }) => {
                    assert!(reason.contains(expected), "{base_url}: {reason}");
                    assert!(!reason.contains("secret-key"), "{reason}");
                }
                other => panic!("{base_url}: expected InvalidProviderUrl, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn moving_the_base_url_drops_the_per_key_trust() {
        let mut existing = sample_key();
        existing.provider = crate::self_hosted::OPENAI_COMPATIBLE.to_string();
        existing.base_url = Some("http://10.20.30.40:52415/v1".to_string());
        existing.trusted_private_address = Some("10.20.30.40:52415".to_string());
        existing.trusted_private_address_set_by = Some(1);
        let mut updated = existing.clone();
        updated.base_url = Some("https://models.example.com/v1".to_string());
        updated.trusted_private_address = None;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![existing]])
                .append_query_results([vec![updated]])
                .into_connection(),
        );
        let service = ProviderKeyService::new(db.clone(), test_encryption());
        // An IP-literal https URL so the change validates without DNS.
        let result = service
            .update(
                1,
                ProviderKeyUpdate {
                    base_url: Some(Some("https://8.8.8.8/v1")),
                    ..Default::default()
                },
            )
            .await;
        assert!(result.is_ok(), "{result:?}");
        drop(service);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(sql.contains("trusted_private_address"), "{sql}");
        assert!(!sql.contains("10.20.30.40:52415"), "{sql}");
    }

    #[tokio::test]
    async fn self_hosted_base_url_cannot_be_cleared() {
        let mut existing = sample_key();
        existing.provider = crate::self_hosted::OPENAI_COMPATIBLE.to_string();
        existing.base_url = Some("http://10.20.30.40:52415/v1".to_string());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![existing]])
            .into_connection();
        let service = ProviderKeyService::new(Arc::new(db), test_encryption());
        let result = service
            .update(
                1,
                ProviderKeyUpdate {
                    base_url: Some(None),
                    ..Default::default()
                },
            )
            .await;
        assert!(
            matches!(result, Err(AiGatewayError::Validation { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn test_decrypt_api_key() {
        let enc = test_encryption();
        let encrypted = enc.encrypt_string("sk-test-key-12345").unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderKeyService::new(Arc::new(db), enc);

        let decrypted = service.decrypt_api_key(&encrypted).unwrap();
        assert_eq!(decrypted, "sk-test-key-12345");
    }

    #[tokio::test]
    async fn deleting_key_with_provider_objects_is_rejected_without_deleting() {
        let object: temps_entities::ai_gateway_objects::Model = serde_json::from_value(serde_json::json!({
            "id":1,"kind":"batch","upstream_id":"batch_test","provider":"openai","provider_key_id":1,
            "credential_scope":"system:1","owner_user_id":1,"owner_project_id":null,"model":null,"endpoint":null,
            "usage_recorded_at":null,"byok_key_encrypted":null,"byok_base_url":null,"next_poll_at":null,"created_at":chrono::Utc::now()
        })).unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![sample_key()]])
                .append_query_results([vec![object]])
                .into_connection(),
        );
        let service = ProviderKeyService::new(db.clone(), test_encryption());
        assert!(matches!(
            service.delete(1).await,
            Err(AiGatewayError::Validation { .. })
        ));
        drop(service);
        let sql = format!("{:?}", Arc::try_unwrap(db).unwrap().into_transaction_log());
        assert!(!sql.contains("DELETE"), "{sql}");
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<ai_provider_keys::Model>::new()])
            .into_connection();

        let service = ProviderKeyService::new(Arc::new(db), test_encryption());
        let result = service.delete(999).await;
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::ProviderKeyNotFound { key_id: 999 }
        ));
    }
}
