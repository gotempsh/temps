// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ProjectEnvVarsProvider implementation for ExternalServiceManager.
//!
//! Wraps the existing per-project service enumeration and attaches service
//! metadata (name/type/slug) so the environments crate can tag each integration
//! env var with the icon + label the UI needs. No new DB queries beyond the
//! lookups already performed by `get_project_service_environment_variables`.

use async_trait::async_trait;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use temps_core::{
    IntegrationEnvVar, IntegrationServiceInfo, ProjectEnvVarsProvider, ProjectIntegrationEnvVars,
};
use temps_entities::{external_services, project_services};

use crate::services::ExternalServiceManager;

/// Adapter exposing `ExternalServiceManager` via the cross-crate trait.
pub struct ExternalServicesEnvProvider {
    manager: Arc<ExternalServiceManager>,
    db: Arc<sea_orm::DatabaseConnection>,
}

impl ExternalServicesEnvProvider {
    pub fn new(manager: Arc<ExternalServiceManager>, db: Arc<sea_orm::DatabaseConnection>) -> Self {
        Self { manager, db }
    }
}

#[async_trait]
impl ProjectEnvVarsProvider for ExternalServicesEnvProvider {
    async fn get_project_integration_env_vars(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<ProjectIntegrationEnvVars>, Box<dyn std::error::Error + Send + Sync>> {
        let per_service = match environment_id {
            Some(env_id) => self
                .manager
                .preview_project_service_environment_variables(project_id, env_id)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
            None => self
                .manager
                .get_project_service_environment_variables(project_id)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
        };

        if per_service.is_empty() {
            // Still return the empty shells for any linked service so the UI
            // can show "Postgres connected, no vars" rather than nothing.
            let linked = project_services::Entity::find()
                .filter(project_services::Column::ProjectId.eq(project_id))
                .all(self.db.as_ref())
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            let service_ids: Vec<i32> = linked.iter().map(|l| l.service_id).collect();
            let services = external_services::Entity::find()
                .filter(external_services::Column::Id.is_in(service_ids))
                .all(self.db.as_ref())
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

            return Ok(services
                .into_iter()
                .map(|s| ProjectIntegrationEnvVars {
                    service: IntegrationServiceInfo {
                        service_id: s.id,
                        service_name: s.name,
                        service_type: s.service_type,
                        service_slug: s.slug,
                        service_updated_at: s.updated_at.to_rfc3339(),
                    },
                    variables: Vec::new(),
                })
                .collect());
        }

        let service_ids: Vec<i32> = per_service.keys().copied().collect();
        let services = external_services::Entity::find()
            .filter(external_services::Column::Id.is_in(service_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        let mut out = Vec::with_capacity(services.len());
        for svc in services {
            let vars = per_service.get(&svc.id).cloned().unwrap_or_default();
            let variables = vars
                .into_iter()
                .map(|(k, v)| IntegrationEnvVar { key: k, value: v })
                .collect();
            out.push(ProjectIntegrationEnvVars {
                service: IntegrationServiceInfo {
                    service_id: svc.id,
                    service_name: svc.name,
                    service_type: svc.service_type,
                    service_slug: svc.slug,
                    service_updated_at: svc.updated_at.to_rfc3339(),
                },
                variables,
            });
        }

        Ok(out)
    }
}
