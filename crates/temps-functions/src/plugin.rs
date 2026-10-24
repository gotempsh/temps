// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Functions plugin registration.
//!
//! Needs the process's activator and its shared workload plane handle,
//! both registered by the binary before plugins (`temps serve`). Registers
//! the function backend with the activator and the existing functions with
//! it (asleep) at startup; it starts no background loop of its own (the
//! activator's sweeper and the plane's reconciler do the work).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, RequiredService, ServiceRegistrationContext,
    TempsPlugin,
};
use temps_core::{AuditLogger, ProjectDeletionHooks};
use temps_workload_activator::{ActivationKind, Activator};
use temps_workload_plane::SharedPlane;
use tracing::{debug, info, warn};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::backend::FunctionActivationBackend;
use crate::handlers::{configure_routes, FunctionsApiDoc, FunctionsAppState};
use crate::service::{FunctionService, SettingsDomain};

pub struct FunctionsPlugin;

impl FunctionsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FunctionsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for FunctionsPlugin {
    fn name(&self) -> &'static str {
        "functions"
    }

    fn required_services(&self) -> Vec<RequiredService> {
        vec![
            RequiredService::of::<DatabaseConnection>(),
            RequiredService::of::<temps_config::ConfigService>(),
            RequiredService::of::<Activator>(),
            RequiredService::of::<SharedPlane>(),
        ]
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            let config = context.require_service::<temps_config::ConfigService>();
            let activator = context.require_service::<Activator>();
            let plane = context.require_service::<SharedPlane>();
            let artifacts_dir = config.data_dir().join("functions");
            activator.set_backend(
                ActivationKind::Function,
                Arc::new(FunctionActivationBackend::new(
                    db.clone(),
                    plane.clone(),
                    &activator,
                )),
            );
            let service = Arc::new(FunctionService::new(
                db,
                plane,
                activator,
                artifacts_dir,
                Arc::new(SettingsDomain::new(config)),
            ));
            context.register_service(service.clone());
            // A project's functions are retired before its rows (and the
            // cascading `functions` rows) are deleted.
            ProjectDeletionHooks::get_or_register(context).add(service);
            debug!("Functions plugin services registered");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let service = context.require_service::<FunctionService>();
            // A failed load leaves functions unrouted until the next start,
            // never the server down; it is logged with why.
            match service.load_into_activator().await {
                Ok(count) => info!(count, "functions registered with the activator (asleep)"),
                Err(error) => warn!("registering functions with the activator failed: {error}"),
            }
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let state = Arc::new(FunctionsAppState {
            service: context.require_service::<FunctionService>(),
            audit_service: context.require_service::<dyn AuditLogger>(),
            project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
        });
        Some(PluginRoutes::new(configure_routes().with_state(state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<FunctionsApiDoc as OpenApiTrait>::openapi())
    }
}
