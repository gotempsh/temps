// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workflows plugin registration (ADR-103).
//!
//! Registers the store, engine, service and scheduler, starts the scheduler
//! once every plugin has registered, and mounts the management routes and the
//! public executor and app routes. Executors are found through
//! [`ExecutorEndpoints`] (registered by the deployments plugin), metrics leave
//! through [`ProjectMetricsSink`] (the OTEL plugin), and large payloads go to
//! [`PayloadBlobStore`] when blob storage is available.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, RequiredService, ServiceRegistrationContext,
    TempsPlugin,
};
use temps_core::{
    AuditLogger, EncryptionService, ExecutorEndpoints, PayloadBlobStore, ProjectMetricsSink,
};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::engine::WorkflowEngine;
use crate::executor::ExecutorClient;
use crate::handlers::{
    configure_public_routes, configure_routes, WorkflowsApiDoc, WorkflowsAppState,
};
use crate::metrics::WorkflowMetrics;
use crate::payload::PayloadCodec;
use crate::scheduler::{SchedulerConfig, WorkflowScheduler};
use crate::service::WorkflowService;
use crate::store::{PgWorkflowStore, WorkflowStore};
use crate::token::InvocationSigner;

#[derive(Default)]
pub struct WorkflowsPlugin;

impl WorkflowsPlugin {
    pub fn new() -> Self {
        Self
    }
}

fn app_state(context: &PluginContext) -> Arc<WorkflowsAppState> {
    let signer = context.require_service::<InvocationSigner>();
    Arc::new(WorkflowsAppState {
        service: context.require_service::<WorkflowService>(),
        engine: context.require_service::<WorkflowEngine>(),
        invocation_key: crate::protocol::InvocationKey::of(&signer),
        audit_service: context.require_service::<dyn AuditLogger>(),
        project_access_checker: context.get_service::<dyn temps_core::ProjectAccessChecker>(),
    })
}

impl TempsPlugin for WorkflowsPlugin {
    fn name(&self) -> &'static str {
        "workflows"
    }

    fn required_services(&self) -> Vec<RequiredService> {
        vec![
            RequiredService::of::<DatabaseConnection>(),
            RequiredService::of::<dyn temps_core::workflow_journal::ManagedWorkflowJournal>(),
            RequiredService::of::<EncryptionService>(),
            RequiredService::of::<dyn ExecutorEndpoints>(),
            RequiredService::of::<dyn ProjectMetricsSink>(),
            RequiredService::of::<dyn AuditLogger>(),
        ]
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();
            let encryption = context.require_service::<EncryptionService>();
            let endpoints = context.require_service::<dyn ExecutorEndpoints>();
            let sink = context.require_service::<dyn ProjectMetricsSink>();
            // Optional: without blob storage, payloads are capped at the
            // inline limit and a larger one is refused with that reason.
            let blobs = context.get_service::<dyn PayloadBlobStore>();
            if blobs.is_none() {
                tracing::info!("blob storage is not available: workflow payloads are limited to the inline size");
            }

            let store: Arc<dyn WorkflowStore> = Arc::new(PgWorkflowStore::new(db.clone()).with_managed_journal(context.require_service::<dyn temps_core::workflow_journal::ManagedWorkflowJournal>()));
            let codec = Arc::new(
                PayloadCodec::new(encryption.clone(), blobs).with_project_keys(Arc::new(
                    crate::project_keys::ProjectKeys::new(db.clone(), encryption.clone()),
                )),
            );
            let signer = Arc::new(InvocationSigner::new(&encryption));
            let metrics = Arc::new(WorkflowMetrics::new(sink));
            let dev = Arc::new(crate::dev::DevTunnel::new(
                db.clone(),
                encryption.clone(),
                signer.clone(),
            ));
            let endpoints = Arc::new(crate::dev::DevEndpoints {
                tunnel: dev.clone(),
                deployed: endpoints,
            });
            let client =
                ExecutorClient::new().map_err(|error| PluginError::PluginRegistrationFailed {
                    plugin_name: "workflows".into(),
                    error: format!("the executor HTTP client could not be built: {error}"),
                })?;
            let engine = Arc::new(WorkflowEngine::new(
                store.clone(),
                codec.clone(),
                signer.clone(),
                endpoints,
                client.with_dev(dev.clone()),
                metrics.clone(),
            ));
            let service = Arc::new(
                WorkflowService::new(
                    store.clone(),
                    engine.clone(),
                    codec.clone(),
                    metrics.clone(),
                )
                .with_dev(dev),
            );
            let listen_pool = match db.as_ref() {
                DatabaseConnection::SqlxPostgresPoolConnection(_) => {
                    Some(db.get_postgres_connection_pool().clone())
                }
                // Not Postgres (tests on a mock): poll only.
                _ => None,
            };
            let control: Arc<dyn temps_core::WorkflowControl> = Arc::new(
                crate::control::WorkflowControlService::new(service.clone(), store.clone()),
            );
            let scheduler = Arc::new(WorkflowScheduler::new(
                engine.clone(),
                store,
                codec,
                metrics,
                listen_pool,
                SchedulerConfig::default(),
            ));

            context.register_service(signer);
            context.register_service(engine);
            context.register_service(service);
            context.register_service(scheduler);
            context.register_service(control);
            tracing::debug!("workflows plugin services registered");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            context.require_service::<WorkflowScheduler>().start();
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        Some(PluginRoutes::new(
            configure_routes().with_state(app_state(context)),
        ))
    }

    fn configure_public_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        Some(PluginRoutes::new(
            configure_public_routes().with_state(app_state(context)),
        ))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<WorkflowsApiDoc as OpenApiTrait>::openapi())
    }
}
