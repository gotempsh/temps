use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, ModelTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};
use temps_entities::{ai_provider_keys, ai_provider_models};

use crate::error::AiGatewayError;
use crate::services::{GatewayService, ProviderKeyService};

pub const MODEL_SOURCE_DISCOVERED: &str = "discovered";
pub const MODEL_SOURCE_MANUAL: &str = "manual";
pub const MODEL_SOURCE_BOOTSTRAP: &str = "bootstrap";

/// Persistent provider-key-scoped model inventory. Keys can have different
/// entitlements or base URLs, so a global provider model list is insufficient.
pub struct ProviderModelService {
    db: Arc<DatabaseConnection>,
    keys: Arc<ProviderKeyService>,
    gateway: Arc<GatewayService>,
}

impl ProviderModelService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        keys: Arc<ProviderKeyService>,
        gateway: Arc<GatewayService>,
    ) -> Self {
        Self { db, keys, gateway }
    }

    pub async fn list_for_key(
        &self,
        provider_key_id: i32,
    ) -> Result<Vec<ai_provider_models::Model>, AiGatewayError> {
        let _ = self.keys.get_by_id(provider_key_id).await?;
        Ok(ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(provider_key_id))
            .order_by_desc(ai_provider_models::Column::IsAvailable)
            .order_by_desc(ai_provider_models::Column::IsEnabled)
            .order_by_asc(ai_provider_models::Column::ModelId)
            .all(self.db.as_ref())
            .await?)
    }

    pub async fn list_active_catalog(
        &self,
    ) -> Result<Vec<crate::types::ModelInfo>, AiGatewayError> {
        let active_key_ids: Vec<i32> = self
            .keys
            .list_active()
            .await?
            .into_iter()
            .map(|key| key.id)
            .collect();
        if active_key_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.is_in(active_key_ids))
            .filter(ai_provider_models::Column::IsEnabled.eq(true))
            .filter(ai_provider_models::Column::IsAvailable.eq(true))
            .order_by_asc(ai_provider_models::Column::ModelId)
            .all(self.db.as_ref())
            .await?;
        let mut models: Vec<crate::types::ModelInfo> = rows
            .into_iter()
            .map(|row| crate::types::ModelInfo {
                id: row.model_id,
                object: "model".to_string(),
                owned_by: row.owned_by.unwrap_or_else(|| "custom".to_string()),
            })
            .collect();
        models.dedup_by(|left, right| left.id == right.id);
        Ok(models)
    }

    pub async fn seed_bootstrap(
        &self,
        key: &ai_provider_keys::Model,
    ) -> Result<(), AiGatewayError> {
        let models = self.gateway.available_models_for_provider(&key.provider);
        if models.is_empty() {
            return Ok(());
        }
        let rows = models
            .into_iter()
            .map(|model| ai_provider_models::ActiveModel {
                provider_key_id: Set(key.id),
                display_name: Set(model.id.clone()),
                model_id: Set(model.id),
                source: Set(MODEL_SOURCE_BOOTSTRAP.to_string()),
                is_available: Set(true),
                is_enabled: Set(true),
                owned_by: Set(Some(model.owned_by)),
                ..Default::default()
            });
        ai_provider_models::Entity::insert_many(rows)
            .on_conflict(
                OnConflict::columns([
                    ai_provider_models::Column::ProviderKeyId,
                    ai_provider_models::Column::ModelId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await?;
        Ok(())
    }

    pub async fn refresh(
        &self,
        provider_key_id: i32,
    ) -> Result<Vec<ai_provider_models::Model>, AiGatewayError> {
        let discovered = self
            .gateway
            .discover_models_for_key(provider_key_id)
            .await?;
        let existing = self.list_for_key(provider_key_id).await?;
        let manual_ids: HashSet<&str> = existing
            .iter()
            .filter(|model| model.source == MODEL_SOURCE_MANUAL)
            .map(|model| model.model_id.as_str())
            .collect();
        let existing_by_id: HashMap<&str, &ai_provider_models::Model> = existing
            .iter()
            .map(|model| (model.model_id.as_str(), model))
            .collect();
        let now = chrono::Utc::now();
        let transaction = self.db.begin().await?;

        ai_provider_models::Entity::update_many()
            .col_expr(
                ai_provider_models::Column::IsAvailable,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(ai_provider_models::Column::ProviderKeyId.eq(provider_key_id))
            .filter(ai_provider_models::Column::Source.ne(MODEL_SOURCE_MANUAL))
            .exec(&transaction)
            .await?;

        for model in discovered {
            if manual_ids.contains(model.id.as_str()) {
                continue;
            }
            if let Some(existing) = existing_by_id.get(model.id.as_str()) {
                let mut active: ai_provider_models::ActiveModel = (*existing).clone().into();
                active.display_name = Set(model.id.clone());
                active.source = Set(MODEL_SOURCE_DISCOVERED.to_string());
                active.is_available = Set(true);
                active.owned_by = Set(Some(model.owned_by));
                active.last_seen_at = Set(Some(now));
                active.update(&transaction).await?;
            } else {
                ai_provider_models::ActiveModel {
                    provider_key_id: Set(provider_key_id),
                    model_id: Set(model.id.clone()),
                    display_name: Set(model.id),
                    source: Set(MODEL_SOURCE_DISCOVERED.to_string()),
                    is_available: Set(true),
                    is_enabled: Set(true),
                    owned_by: Set(Some(model.owned_by)),
                    last_seen_at: Set(Some(now)),
                    ..Default::default()
                }
                .insert(&transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        self.list_for_key(provider_key_id).await
    }

    pub async fn add_manual(
        &self,
        provider_key_id: i32,
        model_id: &str,
        display_name: Option<&str>,
    ) -> Result<ai_provider_models::Model, AiGatewayError> {
        let model_id = validate_model_id(model_id)?;
        let _ = self.keys.get_by_id(provider_key_id).await?;
        let duplicate = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(provider_key_id))
            .filter(ai_provider_models::Column::ModelId.eq(model_id))
            .one(self.db.as_ref())
            .await?;
        if duplicate.is_some() {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Model '{model_id}' already exists for provider key {provider_key_id}"
                ),
            });
        }
        Ok(ai_provider_models::ActiveModel {
            provider_key_id: Set(provider_key_id),
            model_id: Set(model_id.to_string()),
            display_name: Set(display_name.unwrap_or(model_id).trim().to_string()),
            source: Set(MODEL_SOURCE_MANUAL.to_string()),
            is_available: Set(true),
            is_enabled: Set(true),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?)
    }

    pub async fn set_enabled(
        &self,
        provider_key_id: i32,
        model_row_id: i32,
        enabled: bool,
    ) -> Result<ai_provider_models::Model, AiGatewayError> {
        let key = self.keys.get_by_id(provider_key_id).await?;
        let model = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(provider_key_id))
            .filter(ai_provider_models::Column::Id.eq(model_row_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AiGatewayError::ModelNotFound {
                model: model_row_id.to_string(),
            })?;
        if !enabled && key.default_model.as_deref() == Some(model.model_id.as_str()) {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Model '{}' is the default for provider key {provider_key_id}; select another default before disabling it",
                    model.model_id
                ),
            });
        }
        let mut active: ai_provider_models::ActiveModel = model.into();
        active.is_enabled = Set(enabled);
        Ok(active.update(self.db.as_ref()).await?)
    }

    pub async fn ensure_selectable(
        &self,
        provider_key_id: i32,
        model_id: &str,
    ) -> Result<(), AiGatewayError> {
        let selectable = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(provider_key_id))
            .filter(ai_provider_models::Column::ModelId.eq(model_id))
            .filter(ai_provider_models::Column::IsEnabled.eq(true))
            .filter(ai_provider_models::Column::IsAvailable.eq(true))
            .one(self.db.as_ref())
            .await?;
        if selectable.is_none() {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Default model '{model_id}' must be available and enabled for provider key {provider_key_id}"
                ),
            });
        }
        Ok(())
    }

    pub async fn delete_manual(
        &self,
        provider_key_id: i32,
        model_row_id: i32,
    ) -> Result<(), AiGatewayError> {
        let key = self.keys.get_by_id(provider_key_id).await?;
        let model = ai_provider_models::Entity::find()
            .filter(ai_provider_models::Column::ProviderKeyId.eq(provider_key_id))
            .filter(ai_provider_models::Column::Id.eq(model_row_id))
            .filter(ai_provider_models::Column::Source.eq(MODEL_SOURCE_MANUAL))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| AiGatewayError::Validation {
                message: format!("Manual model row {model_row_id} was not found for provider key {provider_key_id}"),
            })?;
        if key.default_model.as_deref() == Some(model.model_id.as_str()) {
            return Err(AiGatewayError::Validation {
                message: format!(
                    "Model '{}' is the default for provider key {provider_key_id}; select another default before deleting it",
                    model.model_id
                ),
            });
        }
        model.delete(self.db.as_ref()).await?;
        Ok(())
    }
}

fn validate_model_id(model_id: &str) -> Result<&str, AiGatewayError> {
    let model_id = model_id.trim();
    if model_id.is_empty() || model_id.len() > 255 || model_id.chars().any(char::is_control) {
        return Err(AiGatewayError::Validation {
            message: "Model ID must contain 1-255 non-control characters".to_string(),
        });
    }
    Ok(model_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn model(model_id: &str, enabled: bool, available: bool) -> ai_provider_models::Model {
        let now = chrono::Utc::now();
        ai_provider_models::Model {
            id: 11,
            provider_key_id: 7,
            model_id: model_id.to_string(),
            display_name: model_id.to_string(),
            source: MODEL_SOURCE_DISCOVERED.to_string(),
            is_available: available,
            is_enabled: enabled,
            owned_by: Some("openai".to_string()),
            metadata: None,
            last_seen_at: Some(now),
            created_at: now,
            updated_at: now,
        }
    }

    fn service_with_results(
        results: impl IntoIterator<Item = Vec<ai_provider_models::Model>>,
    ) -> ProviderModelService {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(results)
                .into_connection(),
        );
        let encryption = Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901").unwrap(),
        );
        let keys = Arc::new(ProviderKeyService::new(db.clone(), encryption));
        let gateway = Arc::new(GatewayService::new(keys.clone()));
        ProviderModelService::new(db, keys, gateway)
    }

    #[test]
    fn manual_model_id_is_trimmed_and_validated() {
        assert_eq!(
            validate_model_id("  vendor/model-v2  ").unwrap(),
            "vendor/model-v2"
        );
        assert!(matches!(
            validate_model_id("\n"),
            Err(AiGatewayError::Validation { .. })
        ));
        assert!(matches!(
            validate_model_id(&"x".repeat(256)),
            Err(AiGatewayError::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn selectable_model_must_be_enabled_and_available() {
        let service = service_with_results([vec![model("gpt-5.6", true, true)], Vec::new()]);
        assert!(service.ensure_selectable(7, "gpt-5.6").await.is_ok());
        assert!(matches!(
            service.ensure_selectable(7, "disabled-model").await,
            Err(AiGatewayError::Validation { .. })
        ));
    }
}
