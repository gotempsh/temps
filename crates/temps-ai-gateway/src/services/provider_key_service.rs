use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::ai_provider_keys;

use crate::error::AiGatewayError;

pub struct ProviderKeyService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
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
        provider: &str,
        display_name: &str,
        api_key: &str,
        base_url: Option<&str>,
        default_model: Option<&str>,
    ) -> Result<ai_provider_keys::Model, AiGatewayError> {
        if provider.is_empty() {
            return Err(AiGatewayError::Validation {
                message: "Provider cannot be empty".to_string(),
            });
        }
        if api_key.is_empty() {
            return Err(AiGatewayError::Validation {
                message: "API key cannot be empty".to_string(),
            });
        }

        let encrypted_key = self
            .encryption_service
            .encrypt_string(api_key)
            .map_err(|e| AiGatewayError::Encryption(e.to_string()))?;

        let model = ai_provider_keys::ActiveModel {
            provider: Set(provider.to_string()),
            display_name: Set(display_name.to_string()),
            api_key_encrypted: Set(encrypted_key),
            base_url: Set(base_url.map(|s| s.to_string())),
            default_model: Set(default_model.map(|s| s.to_string())),
            is_active: Set(true),
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

    /// Decrypt the API key from the stored encrypted value
    pub fn decrypt_api_key(&self, encrypted: &str) -> Result<String, AiGatewayError> {
        self.encryption_service
            .decrypt_string(encrypted)
            .map_err(|e| AiGatewayError::Encryption(e.to_string()))
    }

    pub async fn update(
        &self,
        id: i32,
        display_name: Option<&str>,
        api_key: Option<&str>,
        base_url: Option<Option<&str>>,
        default_model: Option<Option<&str>>,
        is_active: Option<bool>,
    ) -> Result<ai_provider_keys::Model, AiGatewayError> {
        let existing = self.get_by_id(id).await?;

        let mut active: ai_provider_keys::ActiveModel = existing.into();

        if let Some(name) = display_name {
            active.display_name = Set(name.to_string());
        }

        if let Some(key) = api_key {
            let encrypted = self
                .encryption_service
                .encrypt_string(key)
                .map_err(|e| AiGatewayError::Encryption(e.to_string()))?;
            active.api_key_encrypted = Set(encrypted);
        }

        if let Some(url) = base_url {
            active.base_url = Set(url.map(|s| s.to_string()));
        }

        if let Some(model) = default_model {
            active.default_model = Set(model.map(|s| s.to_string()));
        }

        if let Some(active_flag) = is_active {
            active.is_active = Set(active_flag);
        }

        let result = active.update(self.db.as_ref()).await?;

        Ok(result)
    }

    pub async fn delete(&self, id: i32) -> Result<(), AiGatewayError> {
        let _ = self.get_by_id(id).await?;
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

        let result = service.create("", "Test", "sk-123", None, None).await;
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_create_validates_empty_api_key() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderKeyService::new(Arc::new(db), test_encryption());

        let result = service.create("openai", "Test", "", None, None).await;
        assert!(matches!(
            result.unwrap_err(),
            AiGatewayError::Validation { .. }
        ));
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
