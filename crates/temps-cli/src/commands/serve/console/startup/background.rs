// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 6: background services that need the initialised plugins
//! (certificate renewal, disk, alarms, metrics, service health) and the
//! adapters wired into Traefik discovery.

use super::*;

/// Start the console's background services.
pub(super) async fn start_background_services(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
    config: &Arc<ServerConfig>,
    docker_handle: &Arc<temps_core::DockerHandle>,
    traefik_discovery: &Arc<temps_deployer::traefik_discovery::TraefikDiscoveryHandle>,
) {
    spawn_certificate_renewal_scheduler(service_context);

    spawn_disk_space_monitor(service_context);

    wire_traefik_discovery(service_context, traefik_discovery);

    start_alarm_monitoring(service_context, db, config).await;

    start_external_service_health_monitor(service_context, db, docker_handle);

    // OTel background tasks: anomaly detection and health computation require
    // iterating over active project IDs, which will be wired up when project
    // discovery is integrated. The rate limiter is self-cleaning (evicts on check).
    if service_context
        .get_service::<temps_otel::OtelService>()
        .is_some()
    {
        debug!("OTel plugin registered successfully; background tasks pending project discovery integration");
    }
}

/// Start the certificate renewal scheduler when `TlsService` is registered.
fn spawn_certificate_renewal_scheduler(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
) {
    // NOTE: The backup scheduler is started by `BackupPlugin` during plugin
    // initialization (see `temps-backup/src/plugin.rs`). Do NOT start it here as
    // well -- spawning a second scheduler loop makes both loops independently
    // find each due `backup_schedules` row and enqueue a `Job::BackupRequested`,
    // producing two completed backup runs per service at the same timestamp.

    // Start certificate renewal scheduler (optional - fails gracefully if TlsService unavailable)
    if let Some(tls_service) = service_context.get_service::<temps_domains::TlsService>() {
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        let scheduler_token = cancellation_token.clone();
        let scheduler_service = tls_service.clone();
        let scheduler_telemetry = service_context
            .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
            .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));

        tokio::spawn(async move {
            debug!("Starting certificate renewal scheduler");
            // Catch any panics to prevent scheduler issues from crashing the main task
            let result = std::panic::AssertUnwindSafe(async {
                scheduler_service
                    .start_certificate_renewal_scheduler(scheduler_token, scheduler_telemetry)
                    .await
            })
            .catch_unwind()
            .await;

            match result {
                Ok(Ok(())) => {
                    debug!("Certificate renewal scheduler completed normally");
                }
                Ok(Err(e)) => {
                    tracing::warn!("Certificate renewal scheduler error (non-fatal): {}", e);
                }
                Err(_) => {
                    tracing::warn!(
                        "Certificate renewal scheduler panicked (non-fatal) - scheduler stopped"
                    );
                }
            }
        });

        debug!("Certificate renewal scheduler started in background");
    } else {
        tracing::warn!(
            "TlsService not available - certificate renewal scheduler disabled. \
             This is non-fatal but automatic certificate renewal will not work."
        );
    }
}

/// Start disk space monitoring when `ConfigService` and `AlarmService` are
/// registered.
fn spawn_disk_space_monitor(service_context: &temps_core::plugin::ServiceRegistrationContext) {
    // Start disk space monitoring if ConfigService and AlarmService are available
    if let (Some(config_service), Some(alarm_service)) = (
        service_context.get_service::<temps_config::ConfigService>(),
        service_context.get_service::<AlarmService>(),
    ) {
        let monitor = Arc::new(DiskSpaceMonitor::new(config_service.clone(), alarm_service));

        tokio::spawn(async move {
            monitor.start_monitoring().await;
        });

        debug!("Disk space monitoring started in background");
    } else {
        tracing::warn!(
            "ConfigService or AlarmService not available - disk space monitoring disabled."
        );
    }
}

/// Connect Traefik label discovery to the alarm panel and to TLS
/// provisioning (ADR-041 §2a, §8).
fn wire_traefik_discovery(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    traefik_discovery: &Arc<temps_deployer::traefik_discovery::TraefikDiscoveryHandle>,
) {
    // ADR-041 §2a: wire the alarm service into the Traefik discovery watcher so
    // certificate-drift events are visible in the alarm panel, not just in logs.
    // This must run after MonitoringPlugin (step 9.8) has registered AlarmService.
    // We bridge via `DriftAlarmSink` to avoid a dependency cycle
    // (temps-monitoring depends on temps-deployer).
    if let Some(alarm_service) = service_context.get_service::<AlarmService>() {
        traefik_discovery.inject_alarm_sink(Arc::new(AlarmServiceDriftSink(alarm_service)));
    }

    // ADR-041 §8: wire the TLS provisioner for Traefik label discovery.
    //
    // `temps-deployments` cannot depend on `temps-domains` directly (that would
    // introduce a dependency cycle), so the trait `DiscoveredHostTlsProvisioner`
    // is declared in `temps-deployments` and implemented here — in the serve
    // wiring layer that already depends on both crates — following the same
    // adapter pattern used above for `AlarmServiceDriftSink`.
    //
    // Both DomainService and CertificateRepository are registered by
    // DomainsPlugin::register_services, which has already run by this point
    // (all plugins run register_services before initialize_plugins returns).
    // If DomainsPlugin is absent the require_service below will panic at
    // startup with a clear error, which is the correct behaviour for a missing
    // required dependency (CLAUDE.md: "Use `Arc<T>` and fail at startup if missing").
    {
        use temps_deployments::services::traefik_discovery_service::DiscoveredHostTlsProvisioner;
        use temps_domains::tls::repository::CertificateRepository;
        use temps_domains::DomainService;

        let domain_service = service_context.require_service::<DomainService>();
        let cert_repo = service_context.require_service::<dyn CertificateRepository>();
        let config_service_for_provisioner =
            service_context.require_service::<temps_config::ConfigService>();
        let dns_provider_service_for_provisioner =
            service_context.require_service::<temps_dns::services::DnsProviderService>();
        let provisioner: Arc<dyn DiscoveredHostTlsProvisioner> = Arc::new(TraefikTlsProvisioner {
            domain_service,
            cert_repo,
            config_service: config_service_for_provisioner,
            dns_provider_service: dns_provider_service_for_provisioner,
        });
        service_context.register_service(provisioner);
        debug!("Traefik TLS provisioner (ADR-041 §8) registered");
    }
}

/// Start outage detection, container health monitoring and the metrics
/// pipeline, which all share the `AlarmService` MonitoringPlugin registered.
async fn start_alarm_monitoring(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
    config: &Arc<ServerConfig>,
) {
    // Start alarm service, outage detection, and container health monitoring.
    // AlarmService is already constructed and registered by MonitoringPlugin
    // (step 9.8 above). We retrieve the same Arc here so the background loops
    // share the single instance with the HTTP handlers.
    if let (Some(notification_service), Some(queue_service), Some(alarm_service)) = (
        service_context.get_service::<dyn temps_core::notifications::NotificationService>(),
        service_context.get_service::<dyn temps_core::JobQueue>(),
        service_context.get_service::<AlarmService>(),
    ) {
        // alarm_service is the same Arc<AlarmService> registered by MonitoringPlugin.

        // Start event-driven outage detection (listens to StatusCheckCompleted jobs).
        // The job queue is attached so monitoring.downtime workflows can be fired
        // automatically when an outage is detected.
        let outage_service = Arc::new(
            OutageDetectionService::new(db.clone(), notification_service, alarm_service.clone())
                .with_job_queue(queue_service.clone()),
        );

        let job_receiver = queue_service.subscribe();
        tokio::spawn(async move {
            outage_service.start_monitoring(job_receiver).await;
        });

        debug!("Event-driven outage detection service started (listening to StatusCheckCompleted jobs)");

        // Start container health monitoring (restart count, resource usage)
        start_container_health_monitor(service_context, db, config, &alarm_service).await;

        // Start MetricsScraper for external service DB-level metrics
        // (postgres, redis, mongodb) when monitoring is enabled.
        start_metrics_pipeline(service_context, db, config, &alarm_service).await;
    } else {
        tracing::warn!(
            "NotificationService or JobQueue not available - outage detection and alarm service disabled."
        );
    }
}

/// Start the container health monitor, writing container resource metrics
/// to the configured metrics store when there is one.
async fn start_container_health_monitor(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
    config: &Arc<ServerConfig>,
    alarm_service: &Arc<AlarmService>,
) {
    // Build the metrics store if monitoring is enabled, so container
    // resource metrics are written alongside the alarm logic.
    let container_metrics_store: Option<Arc<dyn temps_metrics::MetricsStore>> = {
        use temps_config::MetricsStoreKind;
        use temps_metrics::{MetricsStore, TimescaleMetricsStore};

        // Provide a metrics store for container resource metrics. When
        // the monitoring store is ClickHouse and CH is configured, use
        // it; otherwise (TimescaleDb, or CH selected but unconfigured)
        // fall back to TimescaleDB.
        match service_context.get_service::<temps_config::ConfigService>() {
            Some(cfg_svc) => match cfg_svc.get_settings().await {
                Ok(settings) => match settings.monitoring.store {
                    MetricsStoreKind::TimescaleDb => {
                        Some(Arc::new(TimescaleMetricsStore::new(db.clone()))
                            as Arc<dyn MetricsStore>)
                    }
                    MetricsStoreKind::ClickHouse => {
                        build_ch_metrics_store(config, cfg_svc.clickhouse_compression()).or_else(
                            || {
                                Some(Arc::new(TimescaleMetricsStore::new(db.clone()))
                                    as Arc<dyn MetricsStore>)
                            },
                        )
                    }
                },
                _ => None,
            },
            None => None,
        }
    };

    let runtime_resolver = service_context.require_service::<dyn ContainerRuntimeResolver>();
    let mut health_monitor = ContainerHealthMonitor::new(
        db.clone(),
        runtime_resolver,
        alarm_service.clone(),
        ContainerHealthConfig::default(),
    );

    if let Some(ms) = container_metrics_store {
        health_monitor = health_monitor.with_metrics_store(ms);
    }

    let health_monitor = Arc::new(health_monitor);
    tokio::spawn(async move {
        health_monitor.start().await;
    });

    debug!("Container health monitor started (poll interval: 30s)");
}

/// Register the metrics store and start the worker metrics collector, the
/// `MetricsScraper`, the `AlertEvaluator` and the hourly raw-metrics prune.
async fn start_metrics_pipeline(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
    config: &Arc<ServerConfig>,
    alarm_service: &Arc<AlarmService>,
) {
    if let (Some(cfg_svc), Some(enc_svc)) = (
        service_context.get_service::<temps_config::ConfigService>(),
        service_context.get_service::<temps_core::EncryptionService>(),
    ) {
        use temps_config::MetricsStoreKind;
        use temps_metrics::{MetricsScraper, MetricsStore, TimescaleMetricsStore};

        // The metrics store and scraper are ALWAYS wired up — the per-service
        // `metrics_enabled` flag is the single source of truth for what gets
        // scraped. The scraper idles (near-zero cost) when no service has
        // monitoring enabled, so a user clicking "Enable Monitoring" on a
        // service just works without an operator first flipping a global flag.
        //
        // When the monitoring store is ClickHouse AND TEMPS_CLICKHOUSE_* is
        // configured, this single binding becomes the ClickHouse store used
        // by the HTTP query endpoints, the AlertEvaluator, AND the scraper's
        // writes. Otherwise (TimescaleDb, or CH selected but unconfigured)
        // it falls back to TimescaleDB unchanged.
        match cfg_svc.get_settings().await {
            Ok(settings) => {
                let metrics_store: Arc<dyn MetricsStore> = match settings.monitoring.store {
                    MetricsStoreKind::ClickHouse => {
                        build_ch_metrics_store(config, cfg_svc.clickhouse_compression())
                            .unwrap_or_else(|| Arc::new(TimescaleMetricsStore::new(db.clone())))
                    }
                    MetricsStoreKind::TimescaleDb => {
                        Arc::new(TimescaleMetricsStore::new(db.clone()))
                    }
                };

                // Register the metrics store so plugins (e.g. providers)
                // can retrieve it for HTTP query endpoints.
                service_context.register_service(metrics_store.clone());

                // ADR 061: pull each worker's edge-proxy and host metrics
                // into the same store, under the worker's node id. The
                // board is what the console reads to explain a worker
                // with no data (e.g. an agent too old to report).
                let worker_metrics_status =
                    Arc::new(temps_metrics::WorkerMetricsStatusBoard::new());
                service_context.register_service(worker_metrics_status.clone());
                let worker_metrics = temps_deployments::services::worker_metrics_collector::WorkerMetricsCollector::new(
                    db.clone(),
                    metrics_store.clone(),
                    cfg_svc.clone(),
                    enc_svc.clone(),
                    worker_metrics_status,
                );
                tokio::spawn(worker_metrics.run());

                let mut scraper =
                    MetricsScraper::new(db.clone(), metrics_store.clone(), cfg_svc, enc_svc);
                // HA clusters are scraped at their elected primary, which
                // only the providers layer can resolve.
                match service_context.get_service::<temps_providers::ExternalServiceManager>() {
                    Some(manager) => {
                        scraper = scraper.with_cluster_primary_connector(manager);
                    }
                    None => warn!(
                        "ExternalServiceManager not available - HA cluster database metrics \
                         will not be collected"
                    ),
                }
                let scraper = Arc::new(scraper);

                tokio::spawn(async move {
                    scraper.start().await;
                });

                debug!("MetricsScraper started (scrapes only services with metrics_enabled=true)");

                // Start AlertEvaluator alongside the scraper.
                // It reads monitoring_alert_rules from the DB and evaluates
                // each rule against the most-recent value from MetricsStore,
                // firing/resolving alarms via the shared AlarmService.
                let evaluator = Arc::new(temps_monitoring::AlertEvaluator::new(
                    db.clone(),
                    metrics_store,
                    alarm_service.clone(),
                ));

                tokio::spawn(async move {
                    evaluator.start().await;
                });

                debug!("AlertEvaluator started (metric threshold alerts, 30s interval)");

                // Start hourly pruning job for raw service_metrics rows.
                // Continuous aggregates (hourly/daily rollups) have their
                // own TimescaleDB retention policies; this only handles
                // the raw hypertable rows.
                {
                    use chrono::{Duration, Utc};
                    use temps_config::MetricsStoreKind;
                    use temps_metrics::{MetricsStore, TimescaleMetricsStore};

                    let prune_db = db.clone();
                    let prune_cfg = service_context.get_service::<temps_config::ConfigService>();
                    // Clone the server config so the ClickHouse arm can build
                    // a store inside the spawned loop without re-spawning the
                    // migration task each tick (CH prune is a TTL-backed no-op).
                    let prune_config = config.clone();

                    tokio::spawn(async move {
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(3600));
                        loop {
                            interval.tick().await;
                            let Some(ref cfg_svc) = prune_cfg else {
                                break;
                            };
                            let settings = match cfg_svc.get_settings().await {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::warn!("PruneMetrics: failed to read settings: {e}");
                                    continue;
                                }
                            };
                            // When ClickHouse is the active metrics store, the
                            // table's native TTL enforces retention — there is
                            // nothing for prune() to do. Skip the tick entirely
                            // rather than build a CH store just to call a no-op.
                            if matches!(settings.monitoring.store, MetricsStoreKind::ClickHouse)
                                && prune_config.is_clickhouse_enabled()
                            {
                                continue;
                            }
                            let store: Arc<dyn MetricsStore> = match settings.monitoring.store {
                                MetricsStoreKind::TimescaleDb => {
                                    Arc::new(TimescaleMetricsStore::new(prune_db.clone()))
                                }
                                MetricsStoreKind::ClickHouse => {
                                    // CH selected but unconfigured — match the
                                    // read/write path's TimescaleDB fallback.
                                    // (The CH-configured case is skipped above.)
                                    Arc::new(TimescaleMetricsStore::new(prune_db.clone()))
                                }
                            };
                            let cutoff = Utc::now()
                                - Duration::days(settings.monitoring.retention_raw_days as i64);
                            match store.prune(cutoff).await {
                                Ok(n) => {
                                    debug!(
                                        "PruneMetrics: pruned {} raw metric rows older than {}",
                                        n, cutoff
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!("PruneMetrics: prune failed: {e}");
                                }
                            }
                        }
                    });

                    debug!("Metrics pruning job scheduled (hourly)");
                }
            }
            Err(e) => {
                tracing::warn!("Failed to read monitoring settings: {e} — MetricsScraper and AlertEvaluator not started");
            }
        }
    } else {
        debug!("ConfigService or EncryptionService not available — MetricsScraper and AlertEvaluator not started");
    }
}

/// Start the external service health monitor (Postgres/Redis/MongoDB/RustFS
/// TCP probes) and register it for the manual-trigger endpoint.
fn start_external_service_health_monitor(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
    docker_handle: &Arc<temps_core::DockerHandle>,
) {
    // Start external service health monitoring (Postgres/Redis/MongoDB/RustFS TCP probes)
    if let (Some(alarm_service), Some(external_service_manager)) = (
        service_context.get_service::<AlarmService>(),
        service_context.get_service::<temps_providers::ExternalServiceManager>(),
    ) {
        use temps_providers::health_monitor::{
            ExternalServiceHealthConfig, ExternalServiceHealthMonitor,
        };
        let mut health_monitor = ExternalServiceHealthMonitor::new(
            db.clone(),
            external_service_manager,
            alarm_service,
            ExternalServiceHealthConfig::default(),
            docker_handle.clone(),
            service_context.require_service::<temps_core::EncryptionService>(),
        );

        // Attach the shared metrics store (registered by the MetricsScraper
        // block above) so the monitor records container CPU/memory history
        // for services with metrics enabled.
        if let Some(metrics_store) =
            service_context.get_service::<dyn temps_metrics::MetricsStore>()
        {
            health_monitor = health_monitor.with_metrics_store(metrics_store);
        }

        let health_monitor = Arc::new(health_monitor);

        // Register so the providers plugin can pick it up and expose a
        // manual-trigger endpoint that reuses the monitor's check logic.
        service_context.register_service(health_monitor.clone());

        let loop_handle = health_monitor.clone();
        tokio::spawn(async move {
            loop_handle.start().await;
        });

        debug!("External service health monitor started (poll interval: 30s)");
    } else {
        tracing::warn!(
            "AlarmService or ExternalServiceManager not available - external service health monitoring disabled."
        );
    }
}
