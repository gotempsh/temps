use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_entities::{project_mcp_definitions, project_skill_definitions};

use crate::error::AgentError;

const ENCRYPTED_VALUE_PREFIX: &str = "temps-encrypted-v1:";
const MASKED_VALUE: &str = "***";

/// Validate a slug used as a URL segment AND as a filesystem path component
/// (skills are extracted to `/home/temps/.claude/skills/<slug>/`, MCP archives
/// will land in `/home/temps/.mcp/<slug>/`). Must reject `..`, `/`, and
/// other characters that could escape the intended directory.
fn validate_slug(slug: &str, kind: &str) -> Result<(), AgentError> {
    if slug.is_empty() {
        return Err(AgentError::Validation {
            message: format!("{kind} slug cannot be empty"),
        });
    }
    if slug.len() > 63 {
        return Err(AgentError::Validation {
            message: format!("{kind} slug must be 63 characters or fewer"),
        });
    }
    let first = slug.as_bytes()[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(AgentError::Validation {
            message: format!("{kind} slug must start with a lowercase letter or digit"),
        });
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(AgentError::Validation {
            message: format!(
                "{kind} slug must contain only lowercase letters, digits, and hyphens"
            ),
        });
    }
    Ok(())
}

// ── Skill Definitions ──

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct CreateSkillDefinitionRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    /// Tar.gz archive of the skill directory (SKILL.md + supporting files).
    #[serde(skip)]
    #[schema(value_type = Option<String>, format = "binary")]
    pub archive: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UpdateSkillDefinitionRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    /// Tar.gz archive of the skill directory.
    #[serde(skip)]
    #[schema(value_type = Option<String>, format = "binary")]
    pub archive: Option<Vec<u8>>,
}

// ── MCP Definitions ──

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct CreateMcpDefinitionRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    /// MCP server config in Claude Code settings.json format.
    /// e.g. { "command": "npx", "args": ["-y", "@agentbrowser/mcp"] }
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UpdateMcpDefinitionRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub config: Option<serde_json::Value>,
}

pub struct DefinitionService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl DefinitionService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    fn transform_sensitive_config_values<F>(
        config: &mut serde_json::Value,
        mut transform: F,
    ) -> Result<(), AgentError>
    where
        F: FnMut(&str, &str) -> Result<String, AgentError>,
    {
        let config = config
            .as_object_mut()
            .ok_or_else(|| AgentError::Validation {
                message: "MCP config must be an object".to_string(),
            })?;

        if let Some(url_value) = config.get_mut("url") {
            let url = url_value.as_str().ok_or_else(|| AgentError::Validation {
                message: "MCP config 'url' must be a string".to_string(),
            })?;
            *url_value = serde_json::Value::String(transform("url", url)?);
        }

        for section_name in ["env", "headers"] {
            let Some(section_value) = config.get_mut(section_name) else {
                continue;
            };
            let section = section_value
                .as_object_mut()
                .ok_or_else(|| AgentError::Validation {
                    message: format!(
                        "MCP config '{}' must be an object of string values",
                        section_name
                    ),
                })?;
            for (name, value) in section {
                let raw = value.as_str().ok_or_else(|| AgentError::Validation {
                    message: format!("MCP config '{}.{}' must be a string", section_name, name),
                })?;
                *value = serde_json::Value::String(transform(name, raw)?);
            }
        }
        Ok(())
    }

    fn encrypt_sensitive_config(
        &self,
        config: &serde_json::Value,
    ) -> Result<serde_json::Value, AgentError> {
        let mut protected = config.clone();
        Self::transform_sensitive_config_values(&mut protected, |name, value| {
            if let Some(encrypted) = value.strip_prefix(ENCRYPTED_VALUE_PREFIX) {
                if self.encryption_service.decrypt_string(encrypted).is_ok() {
                    return Ok(value.to_string());
                }
            }
            self.encryption_service
                .encrypt_string(value)
                .map(|encrypted| format!("{ENCRYPTED_VALUE_PREFIX}{encrypted}"))
                .map_err(|error| AgentError::EncryptionError {
                    message: format!("Failed to encrypt MCP config value '{}': {}", name, error),
                })
        })?;
        Ok(protected)
    }

    fn decrypt_sensitive_config(
        &self,
        config: &serde_json::Value,
    ) -> Result<serde_json::Value, AgentError> {
        let mut decrypted = config.clone();
        Self::transform_sensitive_config_values(&mut decrypted, |name, value| {
            let Some(encrypted) = value.strip_prefix(ENCRYPTED_VALUE_PREFIX) else {
                // Backwards compatibility for rows created before MCP config
                // encryption was introduced. Startup backfill protects them.
                return Ok(value.to_string());
            };
            self.encryption_service
                .decrypt_string(encrypted)
                .map_err(|error| AgentError::EncryptionError {
                    message: format!("Failed to decrypt MCP config value '{}': {}", name, error),
                })
        })?;
        Ok(decrypted)
    }

    pub fn mask_sensitive_config(config: &mut serde_json::Value) {
        let Some(config_object) = config.as_object_mut() else {
            *config = serde_json::Value::String(MASKED_VALUE.to_string());
            return;
        };
        if let Some(url) = config_object.get_mut("url") {
            if url.as_str().is_some_and(str::is_empty) {
                *url = serde_json::Value::String(String::new());
            } else {
                *url = serde_json::Value::String(MASKED_VALUE.to_string());
            }
        }
        for section_name in ["env", "headers"] {
            let Some(section_value) = config_object.get_mut(section_name) else {
                continue;
            };
            let Some(section) = section_value.as_object_mut() else {
                *section_value = serde_json::Value::String(MASKED_VALUE.to_string());
                continue;
            };
            for value in section.values_mut() {
                if value.as_str().is_some_and(str::is_empty) {
                    *value = serde_json::Value::String(String::new());
                } else {
                    *value = serde_json::Value::String(MASKED_VALUE.to_string());
                }
            }
        }
    }

    fn merge_masked_config(
        existing: &serde_json::Value,
        replacement: &mut serde_json::Value,
        path: &str,
    ) -> Result<(), AgentError> {
        match (existing, replacement) {
            (serde_json::Value::Object(existing), serde_json::Value::Object(replacement)) => {
                for (key, replacement_value) in replacement {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if let Some(existing_value) = existing.get(key) {
                        Self::merge_masked_config(existing_value, replacement_value, &child_path)?;
                    } else if Self::contains_masked_config_value(replacement_value) {
                        return Err(AgentError::Validation {
                            message: format!(
                                "Masked MCP config value at '{}' has no existing value to preserve",
                                child_path
                            ),
                        });
                    }
                }
            }
            (existing, serde_json::Value::String(value)) if value == MASKED_VALUE => {
                *value = existing
                    .as_str()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| existing.to_string());
            }
            _ => {}
        }
        Ok(())
    }

    fn contains_masked_config_value(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(value) => value == MASKED_VALUE,
            serde_json::Value::Object(values) => {
                values.values().any(Self::contains_masked_config_value)
            }
            serde_json::Value::Array(values) => {
                values.iter().any(Self::contains_masked_config_value)
            }
            _ => false,
        }
    }

    fn decrypt_mcp_model(
        &self,
        mut model: project_mcp_definitions::Model,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        model.config = self.decrypt_sensitive_config(&model.config)?;
        Ok(model)
    }

    // ── Skills (project-scoped) ──

    pub async fn list_skills(
        &self,
        project_id: i32,
    ) -> Result<Vec<project_skill_definitions::Model>, AgentError> {
        let items = project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.eq(project_id))
            .order_by_asc(project_skill_definitions::Column::Name)
            .all(self.db.as_ref())
            .await?;
        Ok(items)
    }

    pub async fn get_skill(
        &self,
        project_id: i32,
        slug: &str,
    ) -> Result<project_skill_definitions::Model, AgentError> {
        project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.eq(project_id))
            .filter(project_skill_definitions::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await?
            .ok_or(AgentError::SkillDefinitionNotFound {
                project_id,
                slug: slug.to_string(),
            })
    }

    pub async fn get_skills_by_slugs(
        &self,
        project_id: i32,
        slugs: &[String],
    ) -> Result<Vec<project_skill_definitions::Model>, AgentError> {
        if slugs.is_empty() {
            return Ok(vec![]);
        }
        let items = project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.eq(project_id))
            .filter(project_skill_definitions::Column::Slug.is_in(slugs.iter().map(|s| s.as_str())))
            .all(self.db.as_ref())
            .await?;
        Ok(items)
    }

    pub async fn create_skill(
        &self,
        project_id: i32,
        request: CreateSkillDefinitionRequest,
    ) -> Result<project_skill_definitions::Model, AgentError> {
        validate_slug(&request.slug, "Skill")?;
        if request.content.is_empty() {
            return Err(AgentError::Validation {
                message: "Skill content cannot be empty".into(),
            });
        }

        // Pre-check existence so we return a typed 409 instead of a 500 that
        // leaks the PG unique-constraint name. The `From<DbErr>` fallback
        // still catches the race where two inserts arrive concurrently.
        if project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.eq(project_id))
            .filter(project_skill_definitions::Column::Slug.eq(&request.slug))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(AgentError::SkillDefinitionAlreadyExists {
                project_id: Some(project_id),
                slug: request.slug,
            });
        }

        let active = project_skill_definitions::ActiveModel {
            project_id: Set(Some(project_id)),
            slug: Set(request.slug.clone()),
            name: Set(request.name),
            description: Set(request.description),
            content: Set(request.content),
            archive: Set(request.archive),
            ..Default::default()
        };
        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(|e| fold_skill_dup(e, Some(project_id), &request.slug))?;
        Ok(model)
    }

    pub async fn update_skill(
        &self,
        project_id: i32,
        slug: &str,
        request: UpdateSkillDefinitionRequest,
    ) -> Result<project_skill_definitions::Model, AgentError> {
        let existing = self.get_skill(project_id, slug).await?;
        let mut active: project_skill_definitions::ActiveModel = existing.into();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(description) = request.description {
            active.description = Set(Some(description));
        }
        if let Some(content) = request.content {
            active.content = Set(content);
        }
        if request.archive.is_some() {
            active.archive = Set(request.archive);
        }

        let model = active.update(self.db.as_ref()).await?;
        Ok(model)
    }

    pub async fn delete_skill(&self, project_id: i32, slug: &str) -> Result<(), AgentError> {
        let existing = self.get_skill(project_id, slug).await?;
        let active: project_skill_definitions::ActiveModel = existing.into();
        active.delete(self.db.as_ref()).await?;
        Ok(())
    }

    // ── Skills (global — project_id IS NULL) ──

    pub async fn list_global_skills(
        &self,
    ) -> Result<Vec<project_skill_definitions::Model>, AgentError> {
        let items = project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.is_null())
            .order_by_asc(project_skill_definitions::Column::Name)
            .all(self.db.as_ref())
            .await?;
        Ok(items)
    }

    pub async fn get_global_skill(
        &self,
        slug: &str,
    ) -> Result<project_skill_definitions::Model, AgentError> {
        project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.is_null())
            .filter(project_skill_definitions::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await?
            .ok_or(AgentError::SkillDefinitionNotFound {
                project_id: 0,
                slug: slug.to_string(),
            })
    }

    pub async fn create_global_skill(
        &self,
        request: CreateSkillDefinitionRequest,
    ) -> Result<project_skill_definitions::Model, AgentError> {
        validate_slug(&request.slug, "Skill")?;
        if request.content.is_empty() {
            return Err(AgentError::Validation {
                message: "Skill content cannot be empty".into(),
            });
        }

        if project_skill_definitions::Entity::find()
            .filter(project_skill_definitions::Column::ProjectId.is_null())
            .filter(project_skill_definitions::Column::Slug.eq(&request.slug))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(AgentError::SkillDefinitionAlreadyExists {
                project_id: None,
                slug: request.slug,
            });
        }

        let active = project_skill_definitions::ActiveModel {
            project_id: Set(None),
            slug: Set(request.slug.clone()),
            name: Set(request.name),
            description: Set(request.description),
            content: Set(request.content),
            archive: Set(request.archive),
            ..Default::default()
        };
        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(|e| fold_skill_dup(e, None, &request.slug))?;
        Ok(model)
    }

    pub async fn update_global_skill(
        &self,
        slug: &str,
        request: UpdateSkillDefinitionRequest,
    ) -> Result<project_skill_definitions::Model, AgentError> {
        let existing = self.get_global_skill(slug).await?;
        let mut active: project_skill_definitions::ActiveModel = existing.into();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(description) = request.description {
            active.description = Set(Some(description));
        }
        if let Some(content) = request.content {
            active.content = Set(content);
        }
        if request.archive.is_some() {
            active.archive = Set(request.archive);
        }

        let model = active.update(self.db.as_ref()).await?;
        Ok(model)
    }

    pub async fn delete_global_skill(&self, slug: &str) -> Result<(), AgentError> {
        let existing = self.get_global_skill(slug).await?;
        let active: project_skill_definitions::ActiveModel = existing.into();
        active.delete(self.db.as_ref()).await?;
        Ok(())
    }

    /// Get all skills available to a project: project-scoped + global definitions.
    pub async fn get_all_available_skills(
        &self,
        project_id: i32,
        slugs: &[String],
    ) -> Result<Vec<project_skill_definitions::Model>, AgentError> {
        if slugs.is_empty() {
            return Ok(vec![]);
        }
        let items = project_skill_definitions::Entity::find()
            .filter(
                Condition::any()
                    .add(project_skill_definitions::Column::ProjectId.eq(project_id))
                    .add(project_skill_definitions::Column::ProjectId.is_null()),
            )
            .filter(project_skill_definitions::Column::Slug.is_in(slugs.iter().map(|s| s.as_str())))
            .all(self.db.as_ref())
            .await?;
        Ok(items)
    }

    // ── MCP Servers (project-scoped) ──

    pub async fn list_mcps(
        &self,
        project_id: i32,
    ) -> Result<Vec<project_mcp_definitions::Model>, AgentError> {
        let items = project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.eq(project_id))
            .order_by_asc(project_mcp_definitions::Column::Name)
            .all(self.db.as_ref())
            .await?;
        items
            .into_iter()
            .map(|item| self.decrypt_mcp_model(item))
            .collect()
    }

    pub async fn get_mcp(
        &self,
        project_id: i32,
        slug: &str,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        let model = project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.eq(project_id))
            .filter(project_mcp_definitions::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await?
            .ok_or(AgentError::McpDefinitionNotFound {
                project_id,
                slug: slug.to_string(),
            })?;
        self.decrypt_mcp_model(model)
    }

    pub async fn get_mcps_by_slugs(
        &self,
        project_id: i32,
        slugs: &[String],
    ) -> Result<Vec<project_mcp_definitions::Model>, AgentError> {
        if slugs.is_empty() {
            return Ok(vec![]);
        }
        let items = project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.eq(project_id))
            .filter(project_mcp_definitions::Column::Slug.is_in(slugs.iter().map(|s| s.as_str())))
            .all(self.db.as_ref())
            .await?;
        items
            .into_iter()
            .map(|item| self.decrypt_mcp_model(item))
            .collect()
    }

    pub async fn create_mcp(
        &self,
        project_id: i32,
        request: CreateMcpDefinitionRequest,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        validate_slug(&request.slug, "MCP server")?;

        if project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.eq(project_id))
            .filter(project_mcp_definitions::Column::Slug.eq(&request.slug))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(AgentError::McpDefinitionAlreadyExists {
                project_id: Some(project_id),
                slug: request.slug,
            });
        }

        let protected_config = self.encrypt_sensitive_config(&request.config)?;
        let active = project_mcp_definitions::ActiveModel {
            project_id: Set(Some(project_id)),
            slug: Set(request.slug.clone()),
            name: Set(request.name),
            description: Set(request.description),
            config: Set(protected_config),
            ..Default::default()
        };
        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(|e| fold_mcp_dup(e, Some(project_id), &request.slug))?;
        self.decrypt_mcp_model(model)
    }

    pub async fn update_mcp(
        &self,
        project_id: i32,
        slug: &str,
        request: UpdateMcpDefinitionRequest,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        let existing = self.get_mcp(project_id, slug).await?;
        let existing_config = existing.config.clone();
        let mut active: project_mcp_definitions::ActiveModel = existing.into();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(description) = request.description {
            active.description = Set(Some(description));
        }
        if let Some(mut config) = request.config {
            Self::merge_masked_config(&existing_config, &mut config, "")?;
            active.config = Set(self.encrypt_sensitive_config(&config)?);
        }

        let model = active.update(self.db.as_ref()).await?;
        self.decrypt_mcp_model(model)
    }

    pub async fn delete_mcp(&self, project_id: i32, slug: &str) -> Result<(), AgentError> {
        let existing = self.get_mcp(project_id, slug).await?;
        let active: project_mcp_definitions::ActiveModel = existing.into();
        active.delete(self.db.as_ref()).await?;
        Ok(())
    }

    // ── MCP Servers (global — project_id IS NULL) ──

    pub async fn list_global_mcps(
        &self,
    ) -> Result<Vec<project_mcp_definitions::Model>, AgentError> {
        let items = project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.is_null())
            .order_by_asc(project_mcp_definitions::Column::Name)
            .all(self.db.as_ref())
            .await?;
        items
            .into_iter()
            .map(|item| self.decrypt_mcp_model(item))
            .collect()
    }

    pub async fn get_global_mcp(
        &self,
        slug: &str,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        let model = project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.is_null())
            .filter(project_mcp_definitions::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await?
            .ok_or(AgentError::McpDefinitionNotFound {
                project_id: 0,
                slug: slug.to_string(),
            })?;
        self.decrypt_mcp_model(model)
    }

    pub async fn create_global_mcp(
        &self,
        request: CreateMcpDefinitionRequest,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        validate_slug(&request.slug, "MCP server")?;

        if project_mcp_definitions::Entity::find()
            .filter(project_mcp_definitions::Column::ProjectId.is_null())
            .filter(project_mcp_definitions::Column::Slug.eq(&request.slug))
            .one(self.db.as_ref())
            .await?
            .is_some()
        {
            return Err(AgentError::McpDefinitionAlreadyExists {
                project_id: None,
                slug: request.slug,
            });
        }

        let protected_config = self.encrypt_sensitive_config(&request.config)?;
        let active = project_mcp_definitions::ActiveModel {
            project_id: Set(None),
            slug: Set(request.slug.clone()),
            name: Set(request.name),
            description: Set(request.description),
            config: Set(protected_config),
            ..Default::default()
        };
        let model = active
            .insert(self.db.as_ref())
            .await
            .map_err(|e| fold_mcp_dup(e, None, &request.slug))?;
        self.decrypt_mcp_model(model)
    }

    pub async fn update_global_mcp(
        &self,
        slug: &str,
        request: UpdateMcpDefinitionRequest,
    ) -> Result<project_mcp_definitions::Model, AgentError> {
        let existing = self.get_global_mcp(slug).await?;
        let existing_config = existing.config.clone();
        let mut active: project_mcp_definitions::ActiveModel = existing.into();

        if let Some(name) = request.name {
            active.name = Set(name);
        }
        if let Some(description) = request.description {
            active.description = Set(Some(description));
        }
        if let Some(mut config) = request.config {
            Self::merge_masked_config(&existing_config, &mut config, "")?;
            active.config = Set(self.encrypt_sensitive_config(&config)?);
        }

        let model = active.update(self.db.as_ref()).await?;
        self.decrypt_mcp_model(model)
    }

    pub async fn delete_global_mcp(&self, slug: &str) -> Result<(), AgentError> {
        let existing = self.get_global_mcp(slug).await?;
        let active: project_mcp_definitions::ActiveModel = existing.into();
        active.delete(self.db.as_ref()).await?;
        Ok(())
    }

    /// Get all MCP servers available to a project: project-scoped + global definitions.
    pub async fn get_all_available_mcps(
        &self,
        project_id: i32,
        slugs: &[String],
    ) -> Result<Vec<project_mcp_definitions::Model>, AgentError> {
        if slugs.is_empty() {
            return Ok(vec![]);
        }
        let items = project_mcp_definitions::Entity::find()
            .filter(
                Condition::any()
                    .add(project_mcp_definitions::Column::ProjectId.eq(project_id))
                    .add(project_mcp_definitions::Column::ProjectId.is_null()),
            )
            .filter(project_mcp_definitions::Column::Slug.is_in(slugs.iter().map(|s| s.as_str())))
            .all(self.db.as_ref())
            .await?;
        items
            .into_iter()
            .map(|item| self.decrypt_mcp_model(item))
            .collect()
    }

    pub async fn reveal_mcp_config_value(
        &self,
        project_id: Option<i32>,
        slug: &str,
        field_path: &str,
    ) -> Result<String, AgentError> {
        let model = match project_id {
            Some(project_id) => self.get_mcp(project_id, slug).await?,
            None => self.get_global_mcp(slug).await?,
        };
        if field_path == "url" {
            return model
                .config
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
                .ok_or_else(|| AgentError::McpConfigFieldNotFound {
                    slug: slug.to_string(),
                    field: field_path.to_string(),
                });
        }
        let Some((section, name)) = field_path.split_once('.') else {
            return Err(AgentError::Validation {
                message: format!(
                    "MCP config field '{}' must identify url or one env or headers value",
                    field_path
                ),
            });
        };
        if !matches!(section, "env" | "headers") || name.is_empty() {
            return Err(AgentError::Validation {
                message: format!(
                    "MCP config field '{}' must identify url or one env or headers value",
                    field_path
                ),
            });
        }
        let value = model
            .config
            .get(section)
            .and_then(|section| section.get(name))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::McpConfigFieldNotFound {
                slug: slug.to_string(),
                field: field_path.to_string(),
            })?;
        Ok(value.to_string())
    }

    /// One-time startup protection for MCP definitions created by older
    /// versions that stored env/header values as plaintext.
    pub async fn encrypt_legacy_mcp_configs(&self) -> Result<u64, AgentError> {
        let models = project_mcp_definitions::Entity::find()
            .all(self.db.as_ref())
            .await?;
        let mut pending = Vec::new();
        for model in models {
            let protected = self.encrypt_sensitive_config(&model.config)?;
            if protected != model.config {
                pending.push((model, protected));
            }
        }
        if pending.is_empty() {
            return Ok(0);
        }

        let transaction = self.db.begin().await?;
        let updated = pending.len() as u64;
        for (model, protected) in pending {
            let mut active: project_mcp_definitions::ActiveModel = model.into();
            active.config = Set(protected);
            active.update(&transaction).await?;
        }
        transaction.commit().await?;
        Ok(updated)
    }
}

/// Map a Sea-ORM insert error to a typed `AlreadyExists` variant when the
/// underlying cause is a Postgres unique-constraint violation. The generic
/// `From<DbErr>` impl already detects this but can't attach the authoritative
/// project_id/slug — do that here at the call site and fall through to the
/// default conversion for anything else.
fn fold_skill_dup(err: sea_orm::DbErr, project_id: Option<i32>, slug: &str) -> AgentError {
    if err
        .to_string()
        .contains("duplicate key value violates unique constraint")
    {
        return AgentError::SkillDefinitionAlreadyExists {
            project_id,
            slug: slug.to_string(),
        };
    }
    AgentError::from(err)
}

fn fold_mcp_dup(err: sea_orm::DbErr, project_id: Option<i32>, slug: &str) -> AgentError {
    if err
        .to_string()
        .contains("duplicate key value violates unique constraint")
    {
        return AgentError::McpDefinitionAlreadyExists {
            project_id,
            slug: slug.to_string(),
        };
    }
    AgentError::from(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use serde_json::json;

    fn service() -> DefinitionService {
        DefinitionService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "mcp-definition-test",
            )),
        )
    }

    #[test]
    fn sensitive_mcp_values_are_encrypted_decrypted_and_masked() {
        let service = service();
        let original = json!({
            "url": "https://user:password@mcp.example.test/rpc?token=secret",
            "env": {"API_TOKEN": "secret-token", "EMPTY": ""},
            "headers": {"Authorization": "Bearer secret"}
        });

        let encrypted = service.encrypt_sensitive_config(&original).unwrap();
        assert_ne!(encrypted["url"], original["url"]);
        assert!(encrypted["url"]
            .as_str()
            .unwrap()
            .starts_with(ENCRYPTED_VALUE_PREFIX));
        assert_ne!(encrypted["env"]["API_TOKEN"], "secret-token");
        assert!(encrypted["env"]["API_TOKEN"]
            .as_str()
            .unwrap()
            .starts_with(ENCRYPTED_VALUE_PREFIX));
        assert_eq!(
            service.decrypt_sensitive_config(&encrypted).unwrap(),
            original
        );

        let mut masked = original;
        DefinitionService::mask_sensitive_config(&mut masked);
        assert_eq!(masked["url"], MASKED_VALUE);
        assert_eq!(masked["env"]["API_TOKEN"], MASKED_VALUE);
        assert_eq!(masked["env"]["EMPTY"], "");
        assert_eq!(masked["headers"]["Authorization"], MASKED_VALUE);
    }

    #[test]
    fn masked_mcp_update_preserves_existing_credentials() {
        let existing = json!({
            "url": "https://user:old-secret@mcp.example.test/rpc",
            "env": {"API_TOKEN": "old-secret"},
            "headers": {"Authorization": "Bearer old"}
        });
        let mut replacement = json!({
            "url": "***",
            "env": {"API_TOKEN": "***", "REGION": "eu-west-1"},
            "headers": {"Authorization": "***"}
        });

        DefinitionService::merge_masked_config(&existing, &mut replacement, "")
            .expect("matching masked paths should preserve existing credentials");

        assert_eq!(
            replacement["url"],
            "https://user:old-secret@mcp.example.test/rpc"
        );
        assert_eq!(replacement["env"]["API_TOKEN"], "old-secret");
        assert_eq!(replacement["env"]["REGION"], "eu-west-1");
        assert_eq!(replacement["headers"]["Authorization"], "Bearer old");
    }

    #[test]
    fn masked_mcp_update_rejects_renamed_credential() {
        let existing = json!({
            "env": {"API_TOKEN": "old-secret"},
            "headers": {"Authorization": "Bearer old"}
        });
        let mut replacement = json!({
            "env": {"RENAMED_TOKEN": "***"},
            "headers": {"X-Authorization": "***"}
        });

        let error = DefinitionService::merge_masked_config(&existing, &mut replacement, "")
            .expect_err("a masked value cannot be moved to a new MCP config path");

        assert!(matches!(error, AgentError::Validation { .. }));
        assert!(error.to_string().contains("env.RENAMED_TOKEN"));
    }

    #[test]
    fn masked_mcp_update_rejects_credential_in_new_section() {
        let existing = json!({
            "env": {"API_TOKEN": "old-secret"}
        });
        let mut replacement = json!({
            "env": {"API_TOKEN": "***"},
            "headers": {"Authorization": "***"}
        });

        let error = DefinitionService::merge_masked_config(&existing, &mut replacement, "")
            .expect_err("a masked value cannot be added beneath a new parent path");

        assert!(matches!(error, AgentError::Validation { .. }));
        assert!(error.to_string().contains("headers"));
    }

    #[test]
    fn malformed_sensitive_mcp_values_are_rejected_and_fail_safe_masked() {
        let service = service();
        for malformed in [
            json!("API_TOKEN=plaintext"),
            json!(["Authorization", "Bearer plaintext"]),
            json!({"env": "API_TOKEN=plaintext"}),
            json!({"env": {"API_TOKEN": 12345}}),
            json!({"headers": ["Authorization", "Bearer plaintext"]}),
            json!({"url": 12345}),
        ] {
            assert!(matches!(
                service.encrypt_sensitive_config(&malformed),
                Err(AgentError::Validation { .. })
            ));

            let mut masked = malformed;
            DefinitionService::mask_sensitive_config(&mut masked);
            assert!(!masked.to_string().contains("plaintext"));
            assert!(!masked.to_string().contains("12345"));
        }
    }

    fn mcp_model(config: serde_json::Value) -> project_mcp_definitions::Model {
        project_mcp_definitions::Model {
            id: 7,
            project_id: Some(42),
            slug: "remote-docs".to_string(),
            name: "Remote docs".to_string(),
            description: None,
            config,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn reveal_mcp_config_value_returns_only_the_requested_secret() {
        let encryption_service = Arc::new(temps_core::EncryptionService::new_from_password(
            "mcp-definition-test",
        ));
        let plaintext = json!({
            "url": "https://user:password@mcp.example.test/rpc?token=secret",
            "env": {"API_TOKEN": "env-secret"},
            "headers": {"Authorization": "Bearer header-secret"}
        });
        let encryptor = DefinitionService::new(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            encryption_service.clone(),
        );
        let protected = encryptor.encrypt_sensitive_config(&plaintext).unwrap();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![mcp_model(protected.clone())],
                vec![mcp_model(protected)],
            ])
            .into_connection();
        let service = DefinitionService::new(Arc::new(db), encryption_service);

        let url = service
            .reveal_mcp_config_value(Some(42), "remote-docs", "url")
            .await
            .unwrap();
        let header = service
            .reveal_mcp_config_value(Some(42), "remote-docs", "headers.Authorization")
            .await
            .unwrap();

        assert_eq!(url, plaintext["url"]);
        assert_eq!(header, "Bearer header-secret");
    }

    #[tokio::test]
    async fn reveal_mcp_config_value_rejects_non_sensitive_fields() {
        let config = json!({"command": "npx", "args": ["server-package"]});
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![mcp_model(config)]])
            .into_connection();
        let service = DefinitionService::new(
            Arc::new(db),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "mcp-definition-test",
            )),
        );

        let error = service
            .reveal_mcp_config_value(Some(42), "remote-docs", "command")
            .await
            .unwrap_err();

        assert!(matches!(error, AgentError::Validation { .. }));
    }
}
