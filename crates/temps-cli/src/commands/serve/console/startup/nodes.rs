// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 7: multi-node control: the node API routes and the periodic node
//! health check with failover.

use super::*;

/// Build the node registration and state routes and start the node health
/// check. Returns the routes for the public router.
pub(super) fn start_node_control(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    db: &Arc<DbConnection>,
) -> Router {
    // Multi-node: create NodeService, register node routes, and start health check
    let config_service_for_nodes = service_context.require_service::<temps_config::ConfigService>();
    let node_service = Arc::new(NodeService::new(db.clone()));
    let encryption_service_for_nodes =
        service_context.require_service::<temps_core::EncryptionService>();
    let node_telemetry = service_context
        .get_service::<dyn temps_core::telemetry::TelemetryReporter>()
        .unwrap_or_else(|| Arc::new(temps_core::telemetry::NoopTelemetryReporter));
    let node_app_state = Arc::new(NodeAppState {
        node_service: node_service.clone(),
        db: db.clone(),
        config_service: config_service_for_nodes,
        encryption_service: encryption_service_for_nodes,
        telemetry: node_telemetry,
        rate_limiter: Arc::new(temps_deployments::handlers::nodes::RegistrationRateLimiter::new()),
        plane_identity_rate_limiter: Arc::new(
            temps_deployments::handlers::nodes::RegistrationRateLimiter::new(),
        ),
        enrollment_token_service: Arc::new(temps_config::EnrollmentTokenService::new(db.clone())),
        alarm_service: service_context.get_service::<AlarmService>(),
        audit_service: service_context.require_service::<dyn temps_core::AuditLogger>(),
        metrics_store: service_context.get_service::<dyn temps_metrics::MetricsStore>(),
    });
    let node_routes =
        temps_deployments::handlers::nodes::configure_routes().with_state(node_app_state);
    // ADR-055 §7: the node state stream, served to workers that advertise
    // `state_stream`. The heartbeat handler reads the same service from an
    // extension to record instance reports.
    let node_routes =
        match service_context.get_service::<temps_deployments::services::NodeStateService>() {
            Some(node_state) => node_routes
                .layer(axum::Extension(node_state.clone()))
                .merge(
                    temps_deployments::handlers::node_state::configure_routes().with_state(
                        Arc::new(temps_deployments::handlers::node_state::NodeStateAppState {
                            node_service: node_service.clone(),
                            node_state,
                            audit_service: service_context
                                .require_service::<dyn temps_core::AuditLogger>(),
                        }),
                    ),
                ),
            None => {
                debug!("Node state stream is not registered in this process; workers use /agent/*");
                node_routes
            }
        };

    // Start periodic node health check with failover (every 60s)
    {
        let health_node_service = node_service.clone();
        let health_db = db.clone();
        let deployment_service_for_failover =
            service_context.get_service::<temps_deployments::DeploymentService>();
        let health_alarm_service = service_context.get_service::<AlarmService>();
        let health_config_service = service_context.get_service::<temps_config::ConfigService>();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            // Node silence only counts from when this process started watching
            // heartbeats: while it was down it recorded none.
            let mut heartbeat_watch = HeartbeatWatch::start(chrono::Utc::now());
            loop {
                interval.tick().await;
                let watching_since = heartbeat_watch.tick(chrono::Utc::now());
                // Sample the control plane's own host metrics so the synthetic
                // control-plane node shows live CPU/mem/disk (it has no agent
                // heartbeat). Always runs, independent of alert config.
                refresh_control_plane_metrics();
                // Failover grace period (`None` = automatic failover disabled).
                // If settings can't be read, keep the default rather than
                // failing over faster — or not at all — than the operator expects.
                let failover_after_secs = match &health_config_service {
                    Some(config_service) => match config_service.get_settings().await {
                        Ok(settings) => settings.multi_node.node_failover_after_secs,
                        Err(e) => {
                            tracing::error!(
                                "Node health check: failed to read failover settings, using default: {}",
                                e
                            );
                            temps_config::MultiNodeSettings::default().node_failover_after_secs
                        }
                    },
                    None => temps_config::MultiNodeSettings::default().node_failover_after_secs,
                };
                let offline_ids =
                    check_node_health(&health_node_service, health_db.as_ref(), watching_since)
                        .await;
                if !offline_ids.is_empty() {
                    tracing::info!(
                        "Node health check: marked {} node(s) as offline",
                        offline_ids.len()
                    );
                    // Alert operators that worker node(s) went down (best-effort).
                    if let Some(ref alarm_service) = health_alarm_service {
                        notify_nodes_offline(
                            &offline_ids,
                            &health_node_service,
                            alarm_service,
                            failover_after_secs,
                        )
                        .await;
                    }
                }

                // Fail over nodes that have stayed offline past the grace
                // period. Decoupled from the offline transition above on
                // purpose: a node that merely missed a heartbeat window keeps
                // its workloads, and only a sustained outage moves them.
                if let (Some(after_secs), Some(ref deployment_service)) =
                    (failover_after_secs, &deployment_service_for_failover)
                {
                    let failed_over = failover_due_nodes(
                        after_secs,
                        watching_since,
                        &health_node_service,
                        deployment_service,
                        health_alarm_service.as_ref(),
                    )
                    .await;
                    if !failed_over.is_empty() {
                        tracing::info!(
                            "Node health check: failed over workloads from {} node(s)",
                            failed_over.len()
                        );
                    }
                }

                // Restore the replicas a failover took out of routing on nodes
                // that are sending heartbeats again.
                if let Some(ref deployment_service) = deployment_service_for_failover {
                    let restored =
                        restore_recovered_replicas(&health_node_service, deployment_service).await;
                    if restored > 0 {
                        tracing::info!(
                            "Node health check: queued {} recovery redeploy(s) for recovered node(s)",
                            restored
                        );
                    }
                }

                // Alert on node resource pressure (CPU/mem/disk) against the
                // operator-configurable thresholds in settings.multi_node.
                if let (Some(ref alarm_service), Some(ref config_service)) =
                    (&health_alarm_service, &health_config_service)
                {
                    check_node_resources(health_db.as_ref(), config_service, alarm_service).await;
                    // The control plane isn't a `nodes` row, so it's excluded
                    // from the query above — alert on its own metrics separately.
                    check_control_plane_resources(config_service, alarm_service).await;
                }

                // Transition fully-drained nodes from "draining" to "drained".
                // Without this, a node whose containers have all migrated stays
                // stuck in "draining" forever (it never auto-completes), so the
                // operator can never safely remove it. (ADR-020 WS-5.1 / lifecycle-7)
                let drained_ids = check_drain_completion(&health_node_service).await;
                if !drained_ids.is_empty() {
                    tracing::info!(
                        "Node drain check: {} node(s) completed draining",
                        drained_ids.len()
                    );
                }
            }
        });
        debug!("Node health check scheduler with failover started (every 60s)");
    }

    node_routes
}
