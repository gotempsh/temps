// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use temps_cloud_client::CloudLink;
use temps_config::ConfigService;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use utoipa::{openapi::OpenApi, OpenApi as _};

use crate::{cloud_routes, CloudApiDoc, CloudService};

pub struct CloudPlugin {
    data_dir: PathBuf,
    agent_version: String,
    allow_loopback_development: bool,
}

impl CloudPlugin {
    pub fn new(data_dir: PathBuf, agent_version: impl Into<String>) -> Self {
        Self {
            data_dir,
            agent_version: agent_version.into(),
            allow_loopback_development: false,
        }
    }

    /// Explicit CLI/development configuration. No ambient environment value
    /// may weaken managed-backend URL validation.
    pub fn new_for_loopback_development(
        data_dir: PathBuf,
        agent_version: impl Into<String>,
    ) -> Self {
        Self {
            data_dir,
            agent_version: agent_version.into(),
            allow_loopback_development: true,
        }
    }
}

impl TempsPlugin for CloudPlugin {
    fn name(&self) -> &'static str {
        "cloud"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let config = context.require_service::<ConfigService>();
            let encryption = context.require_service::<temps_core::EncryptionService>();
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let link = if self.allow_loopback_development {
                Arc::new(CloudLink::load_encrypted_for_loopback_development(
                    self.data_dir.clone(),
                    self.agent_version.clone(),
                    encryption.clone(),
                ))
            } else {
                Arc::new(CloudLink::load_encrypted(
                    self.data_dir.clone(),
                    self.agent_version.clone(),
                    encryption.clone(),
                ))
            };
            let service = Arc::new(CloudService::new(
                link.clone(),
                config,
                db,
                encryption.clone(),
                self.allow_loopback_development,
            ));
            context.register_service(link);
            context.register_service(service);
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let service = context.require_service::<CloudService>();
            if cloud_initialization_succeeded(service.initialize().await) {
                service.start_backup_mirror(
                    context.require_service::<sea_orm::DatabaseConnection>(),
                    context.require_service::<temps_core::EncryptionService>(),
                );
                service.start_backup_credential_rotation();
            }
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        Some(PluginRoutes::new(cloud_routes(
            context.require_service::<CloudService>(),
            context.require_service::<dyn temps_core::AuditLogger>(),
        )))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(CloudApiDoc::openapi())
    }
}

fn cloud_initialization_succeeded(result: Result<(), crate::CloudServiceError>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, "Optional Cloud integration failed to initialize; console startup will continue");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_initialization_failure_does_not_become_a_plugin_failure() {
        let initialized =
            cloud_initialization_succeeded(Err(crate::CloudServiceError::InvalidBackend {
                reason: "invalid test backend".to_string(),
            }));

        assert!(!initialized);
    }
}
