use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_backup_core::BackupExecutorBuilder;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::{error, info};
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::{
    engines::{
        control_plane::{ControlPlaneDeps, ControlPlaneEngine},
        mongodb::{MongodbDeps, MongodbEngine},
        postgres_cluster::{PostgresClusterDeps, PostgresClusterEngine},
        postgres_pgdump::{PostgresPgDumpDeps, PostgresPgDumpEngine},
        postgres_walg::{PostgresWalgDeps, PostgresWalgEngine},
        redis::{RedisDeps, RedisEngine},
        s3_mirror::{S3MirrorDeps, S3MirrorEngine},
    },
    handlers::{self, create_backup_app_state, BackupAppState},
    services::{
        sweep_backup_alerts, BackupNotificationAdapter, BackupService, RestoreService,
        S3LifecycleService,
    },
};
use temps_providers::externalsvc::postgres_upgrade::{
    PostgresContainerLifecycle, PreUpgradeBackupProvider,
};
use temps_providers::postgres_lifecycle::PostgresLifecycleAdapter;
use temps_providers::postgres_upgrade_service::PostgresUpgradeService;

/// Backup Plugin: registers backup services + the in-process executor.
pub struct BackupPlugin;

impl BackupPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BackupPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for BackupPlugin {
    fn name(&self) -> &'static str {
        "backup"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let external_service_manager =
                context.require_service::<temps_providers::ExternalServiceManager>();
            let notification_service =
                context.require_service::<temps_notifications::NotificationService>();
            let config_service = context.require_service::<temps_config::ConfigService>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();

            // Create BackupService.
            let backup_service = Arc::new(BackupService::new(
                db.clone(),
                external_service_manager.clone(),
                notification_service.clone(),
                config_service.clone(),
                encryption_service.clone(),
            ));
            context.register_service(backup_service.clone());

            // Create RestoreService — orchestrates generic restore across all
            // engines via the ExternalService trait.
            let restore_service = Arc::new(RestoreService::new(
                db.clone(),
                external_service_manager.clone(),
                encryption_service.clone(),
            ));
            context.register_service(restore_service.clone());

            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();

            // Postgres major-upgrade service.
            let docker = context.require_service::<bollard::Docker>();
            let log_service = context.require_service::<temps_logs::LogService>();
            let backup_provider: Arc<dyn PreUpgradeBackupProvider> = backup_service.clone();
            let lifecycle: Arc<dyn PostgresContainerLifecycle> =
                Arc::new(PostgresLifecycleAdapter::new(
                    db.clone(),
                    docker.clone(),
                    external_service_manager.clone(),
                    encryption_service.clone(),
                ));
            let pg_upgrade_service = Arc::new(PostgresUpgradeService::new(
                db.clone(),
                docker.clone(),
                backup_provider,
                lifecycle,
                log_service,
            ));
            context.register_service(pg_upgrade_service.clone());

            // ── BackupExecutor: registers all 7 engines ──────────────────────
            let executor_max_concurrent = std::env::var("TEMPS_BACKUP_EXECUTOR_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(4);

            // Cast Arc<temps_notifications::NotificationService> to
            // Arc<dyn temps_core::notifications::NotificationService> so the
            // adapter accepts it.
            let core_notif_svc: Arc<dyn temps_core::notifications::NotificationService> =
                notification_service.clone();
            let executor_notifier: Arc<dyn temps_backup_core::BackupFailureNotifier> =
                Arc::new(BackupNotificationAdapter::new(core_notif_svc, db.clone()));

            // Shared workspace JobQueue. Producers (BackupService) publish
            // Job::BackupRequested here; the BackupJobProcessor subscribes
            // and dispatches to the executor.
            let job_queue = context.require_service::<dyn temps_core::JobQueue>();

            let executor = Arc::new(
                BackupExecutorBuilder::new(db.clone())
                    .with_max_concurrent(executor_max_concurrent)
                    .with_notifier(executor_notifier)
                    .with_event_publisher(Arc::clone(&job_queue))
                    .register_engine(Arc::new(ControlPlaneEngine::new(ControlPlaneDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        config_service: config_service.clone(),
                    })))
                    .register_engine(Arc::new(RedisEngine::new(RedisDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        docker: docker.as_ref().clone(),
                    })))
                    .register_engine(Arc::new(PostgresPgDumpEngine::new(PostgresPgDumpDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        docker: docker.as_ref().clone(),
                    })))
                    .register_engine(Arc::new(PostgresWalgEngine::new(PostgresWalgDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        docker: docker.as_ref().clone(),
                    })))
                    .register_engine(Arc::new(PostgresClusterEngine::new(PostgresClusterDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        docker: docker.as_ref().clone(),
                    })))
                    .register_engine(Arc::new(MongodbEngine::new(MongodbDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        docker: docker.as_ref().clone(),
                    })))
                    .register_engine(Arc::new(S3MirrorEngine::new(S3MirrorDeps {
                        db: db.clone(),
                        encryption_service: encryption_service.clone(),
                        docker: docker.as_ref().clone(),
                    })))
                    .build(),
            );

            info!(
                "BackupExecutor: registered 7 engines: control_plane, redis, \
                 postgres_pgdump, postgres_walg, postgres_cluster, mongodb, s3_mirror",
            );

            // Wire the JobQueue into BackupService so trigger paths can
            // publish Job::BackupRequested messages.
            backup_service.set_queue(Arc::clone(&job_queue));

            // Spawn the consumer loop. It subscribes to the workspace
            // queue and dispatches every Job::BackupRequested to the
            // executor (which owns concurrency, cancel tokens, DB writes).
            {
                let processor = temps_backup_core::BackupJobProcessor::new(Arc::clone(&executor));
                let receiver = job_queue.subscribe();
                tokio::spawn(async move {
                    if let Err(e) = processor.run(receiver).await {
                        error!("BackupJobProcessor exited: {}", e);
                    }
                });
                info!("BackupJobProcessor started");
            }

            let telemetry = context
                .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
                .unwrap_or_else(|| {
                    std::sync::Arc::new(temps_core::telemetry::NoopTelemetryReporter)
                });

            // Thread the telemetry reporter into the pg upgrade service so
            // orchestrators it spawns can emit PgMajorUpgradeCompleted.
            pg_upgrade_service.set_telemetry(Arc::clone(&telemetry));

            let backup_app_state_inner = create_backup_app_state(
                backup_service,
                restore_service,
                audit_service,
                pg_upgrade_service,
                db.clone(),
                Arc::clone(&executor),
                telemetry,
            );

            context.register_service(backup_app_state_inner);

            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let encryption_service = context.require_service::<temps_core::EncryptionService>();
            let backup_app_state = context.require_service::<BackupAppState>();
            let executor = Arc::clone(&backup_app_state.backup_executor);

            // Boot-time orphan reconcile: flip any backups left running/pending
            // by the previous process to `failed`. The executor is the sole
            // owner of in-flight tasks, so anything the DB thinks is running
            // when we boot is by definition dead.
            match executor.reconcile_orphans_on_startup().await {
                Ok(n) if n > 0 => info!(
                    flipped = n,
                    "BackupExecutor: flipped orphan in-flight backups to failed on startup",
                ),
                Ok(_) => {}
                Err(e) => error!("BackupExecutor: orphan reconcile failed at startup: {}", e,),
            }

            // Alert watcher: detects overdue schedules and stalled jobs. Fires
            // every 5 minutes; `Skip` so a slow DB doesn't accumulate ticks.
            let alert_db = db.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    match sweep_backup_alerts(alert_db.as_ref()).await {
                        Ok(stats) if stats.has_changes() => info!(
                            opened_overdue = stats.opened_overdue,
                            opened_stalled = stats.opened_stalled,
                            resolved_overdue = stats.resolved_overdue,
                            resolved_stalled = stats.resolved_stalled,
                            "backup alert sweep: state changes detected"
                        ),
                        Ok(_) => {}
                        Err(e) => error!("backup alert sweep failed: {}", e),
                    }
                }
            });

            // Postgres major-upgrade resume.
            let pg_upgrade_service =
                context.require_service::<temps_providers::postgres_upgrade_service::PostgresUpgradeService>();
            match pg_upgrade_service.resume_active_upgrades().await {
                Ok(n) if n > 0 => {
                    info!(resumed = n, "resumed Postgres major upgrades after restart",)
                }
                Ok(_) => {}
                Err(e) => error!("Failed to resume Postgres major upgrades on boot: {}", e),
            }

            // Rollback volume retention sweep.
            let sweeper = pg_upgrade_service.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
                loop {
                    tick.tick().await;
                    match sweeper.sweep_expired_rollback_volumes().await {
                        Ok(0) => {}
                        Ok(n) => info!(
                            removed = n,
                            "swept expired Postgres-upgrade rollback volumes"
                        ),
                        Err(e) => {
                            error!("Rollback-volume sweep failed (will retry next tick): {}", e)
                        }
                    }
                }
            });

            // S3 lifecycle drift reconciler. Walks every S3 source hourly
            // and re-pushes lifecycle rules so manual edits in the AWS
            // console (or missed event-driven reconciles) eventually
            // converge to the desired state. App-side `enforce_retention`
            // is still the primary cleanup path; this only handles drift
            // on the storage provider side.
            let lifecycle_db = db.clone();
            let lifecycle_enc = encryption_service.clone();
            tokio::spawn(async move {
                use sea_orm::EntityTrait;
                // First tick is one interval out — give the rest of the
                // server time to settle before hammering S3.
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
                tick.tick().await;
                let svc = S3LifecycleService::new(lifecycle_db.clone(), lifecycle_enc);
                loop {
                    tick.tick().await;
                    let sources = match temps_entities::s3_sources::Entity::find()
                        .all(lifecycle_db.as_ref())
                        .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            error!(
                                error = %e,
                                "S3 lifecycle sweep: failed to list S3 sources",
                            );
                            continue;
                        }
                    };
                    for source in sources {
                        match svc.reconcile_bucket(source.id).await {
                            Ok(_) => {}
                            Err(e) => {
                                error!(
                                    s3_source_id = source.id,
                                    error = %e,
                                    "S3 lifecycle reconcile failed during sweep",
                                );
                            }
                        }
                    }
                }
            });

            // Start the schedule loop. It ticks at the top of each hour, finds
            // due `backup_schedules`, and calls `executor.spawn` for each.
            let backup_service = context.require_service::<BackupService>();
            let schedule_cancel = tokio_util::sync::CancellationToken::new();
            tokio::spawn({
                let backup_service = Arc::clone(&backup_service);
                let token = schedule_cancel.clone();
                async move {
                    if let Err(e) = backup_service.start_backup_scheduler(token).await {
                        error!("Backup scheduler exited with error: {}", e);
                    }
                }
            });
            drop(schedule_cancel);

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        let backup_app_state = context.require_service::<BackupAppState>();
        let routes = handlers::configure_routes()
            .merge(handlers::pg_upgrade_handler::configure_routes())
            .merge(handlers::restore_handler::configure_routes())
            .with_state(backup_app_state);
        Some(PluginRoutes::new(routes))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        let mut doc = <handlers::backup_handler::BackupApiDoc as OpenApiTrait>::openapi();
        doc.merge(<handlers::pg_upgrade_handler::PgUpgradeApiDoc as OpenApiTrait>::openapi());
        doc.merge(<handlers::restore_handler::RestoreApiDoc as OpenApiTrait>::openapi());
        Some(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_backup_plugin_name() {
        let backup_plugin = BackupPlugin::new();
        assert_eq!(backup_plugin.name(), "backup");
    }
}
