// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service and cluster health: probes, snapshots and health history.

use crate::externalsvc::{HealthProbeStatus, ServiceType};
use crate::services::*;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use temps_entities::{external_service_health_checks, external_services};
use tracing::{info, warn};

/// Format a `tokio_postgres::Error` (or any `std::error::Error`) by
/// walking its `source()` chain. `tokio_postgres::Error::Display` only
/// emits a brief tag like `db error` and hides the actual cause —
/// callers are expected to walk the chain themselves. This helper does
/// it so probe error messages surface the *real* failure (pg_hba miss,
/// auth failure, TLS rejection, etc.) instead of the useless tag.
fn format_pg_error<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut cause: Option<&dyn std::error::Error> = err.source();
    while let Some(c) = cause {
        let s = c.to_string();
        if !s.is_empty() {
            out.push_str(": ");
            out.push_str(&s);
        }
        cause = c.source();
    }
    out
}

fn compute_uptime_percent(entries: &[HealthCheckEntry], window_hours: i64) -> Option<f64> {
    if entries.is_empty() {
        return None;
    }

    let cutoff = chrono::Utc::now() - chrono::Duration::hours(window_hours);
    let mut total = 0usize;
    let mut operational = 0usize;

    for entry in entries {
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&entry.checked_at) else {
            continue;
        };
        if ts.with_timezone(&chrono::Utc) < cutoff {
            continue;
        }
        total += 1;
        if entry.status == "operational" {
            operational += 1;
        }
    }

    if total == 0 {
        None
    } else {
        Some((operational as f64 / total as f64) * 100.0)
    }
}

impl ExternalServiceManager {
    /// Whether the most recent probe found this service operational.
    ///
    /// Reads the verdict `ExternalServiceHealthMonitor` persists on the row
    /// rather than probing inline, so polling this can't stall on a service
    /// that is unreachable. `degraded` reports `false` here; callers that
    /// need the distinction should read the health snapshot instead.
    ///
    /// This used to return a hardcoded `false` while still doing the lookup,
    /// so `GET /external-services/{id}/health` reported every service as
    /// unhealthy — including ones the monitor had just marked operational.
    pub async fn check_service_health(&self, service_id: i32) -> Result<bool> {
        let service = self.get_service(service_id).await?;

        Ok(service.health_status.as_deref() == Some(HealthProbeStatus::Operational.as_str()))
    }

    /// Return the current health status for many services in one query.
    /// Used by the Storage list page to render per-row status dots without
    /// issuing one HTTP request per service.
    pub async fn list_health_statuses(
        &self,
        service_ids: &[i32],
    ) -> Result<Vec<ServiceHealthStatusEntry>, ExternalServiceError> {
        if service_ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = external_services::Entity::find()
            .filter(external_services::Column::Id.is_in(service_ids.to_vec()))
            .all(self.db.as_ref())
            .await?;

        Ok(rows
            .into_iter()
            .map(|r| ServiceHealthStatusEntry {
                service_id: r.id,
                status: r.health_status,
                last_checked_at: r.last_health_check_at.map(|t| t.to_rfc3339()),
                consecutive_failures: r.consecutive_health_failures,
            })
            .collect())
    }

    /// Return the persisted health snapshot for a service (status, last error,
    /// and the most recent check history). Written by
    /// `ExternalServiceHealthMonitor` on each probe cycle.
    pub async fn get_health_snapshot(
        &self,
        service_id: i32,
        history_limit: u64,
    ) -> Result<ServiceHealthSnapshot, ExternalServiceError> {
        let service = self.get_service(service_id).await?;

        let history = external_service_health_checks::Entity::find()
            .filter(external_service_health_checks::Column::ServiceId.eq(service_id))
            .order_by_desc(external_service_health_checks::Column::CheckedAt)
            .paginate(self.db.as_ref(), history_limit.clamp(1, 200))
            .fetch_page(0)
            .await?;

        let recent_checks = history
            .into_iter()
            .map(|row| HealthCheckEntry {
                checked_at: row.checked_at.to_rfc3339(),
                status: row.status,
                response_time_ms: row.response_time_ms,
                error_message: row.error_message,
            })
            .collect::<Vec<_>>();

        // Most recent response time (first entry when sorted DESC).
        let response_time_ms = recent_checks.first().and_then(|c| c.response_time_ms);

        // 24h uptime percentage based on stored history.
        let uptime_24h_percent = compute_uptime_percent(&recent_checks, 24);

        let down_alarm = if service.consecutive_health_failures
            >= crate::health_monitor::CONSECUTIVE_FAILURES_BEFORE_ALERT
        {
            self.open_down_alarm(service_id).await?
        } else {
            None
        };

        Ok(ServiceHealthSnapshot {
            service_id,
            status: service.health_status,
            last_checked_at: service.last_health_check_at.map(|t| t.to_rfc3339()),
            last_error: service.last_health_error,
            consecutive_failures: service.consecutive_health_failures,
            response_time_ms,
            uptime_24h_percent,
            recent_checks,
            down_alarm,
        })
    }

    /// The newest unresolved down alarm the health monitor raised for this
    /// service, if any. An alarm row is what the monitor actually records;
    /// whether a notification reached anyone is not tracked per alarm.
    async fn open_down_alarm(
        &self,
        service_id: i32,
    ) -> Result<Option<ServiceDownAlarm>, ExternalServiceError> {
        use temps_monitoring::alarm_service::{AlarmStatus, AlarmType};

        let alarm = temps_entities::alarms::Entity::find()
            .filter(temps_entities::alarms::Column::ServiceId.eq(service_id))
            .filter(
                temps_entities::alarms::Column::AlarmType
                    .eq(AlarmType::ExternalServiceDown.as_str()),
            )
            .filter(temps_entities::alarms::Column::Status.ne(AlarmStatus::Resolved.as_str()))
            .order_by_desc(temps_entities::alarms::Column::FiredAt)
            .one(self.db.as_ref())
            .await?;

        Ok(alarm.map(|alarm| ServiceDownAlarm {
            alarm_id: alarm.id,
            status: alarm.status,
            fired_at: alarm.fired_at,
            silenced_until: alarm.silenced_until,
        }))
    }

    /// Health-probe a cluster service by fanning out to:
    ///   1. The pg_auto_failover monitor (proves the cluster's control plane
    ///      is alive and we can read state from it).
    ///   2. Each data member's `pgautofailover.node` reported state, read
    ///      *through* the monitor (no per-member network call needed).
    ///
    /// Why not direct `tokio_postgres::connect(member, password)`:
    /// the monitor already aggregates every member's state, and it accepts
    /// `autoctl_node` (SCRAM, with the cluster's generated password) from
    /// any address — the same path the data nodes themselves use to
    /// register, so we know it works.
    ///
    /// Aggregation rules:
    /// - Monitor reachable + every reported data node in a healthy state
    ///   → `Operational`.
    /// - Monitor reachable + at least one data node not healthy
    ///   → `Degraded` (with per-member states listed).
    /// - Monitor unreachable → `Down` (with full error chain).
    /// - No monitor row at all → `Down` ("no monitor in cluster").
    pub async fn probe_cluster(&self, service: &external_services::Model) -> ClusterProbeResult {
        use std::time::{Duration, Instant};

        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

        let members = match self.get_service_members(service.id).await {
            Ok(m) => m,
            Err(e) => {
                return ClusterProbeResult::down(format!(
                    "Failed to load cluster members for service {}: {}",
                    service.id, e
                ));
            }
        };

        let monitor = members.iter().find(|m| is_role_monitor(&m.role));
        let monitor = match monitor {
            Some(m) => m,
            None => {
                return ClusterProbeResult::down(format!(
                    "Cluster service {} has no monitor member",
                    service.id
                ));
            }
        };

        // `monitor.port` is the host port the lifecycle hook published.
        let monitor_host = match control_plane_member_dial_host(
            self.db.as_ref(),
            monitor.node_id,
            &monitor.container_name,
        )
        .await
        {
            Ok(host) => host,
            Err(reason) => {
                return ClusterProbeResult::down(format!(
                    "Cannot reach the monitor of cluster service {}: {reason}",
                    service.id
                ))
            }
        };
        let monitor_port = monitor.port.unwrap_or(5432);

        // SECURITY: `sslmode=require` prevents tokio-postgres from silently
        // accepting a cleartext socket; `autoctl_node` authenticates with
        // SCRAM, which never puts the password itself on the wire. See
        // `monitor_connection_string`.
        let auth = match crate::externalsvc::postgres_cluster::load_cluster_auth_secrets(
            self.db.as_ref(),
            self.encryption_service.as_ref(),
            service.id,
        )
        .await
        {
            Ok(auth) => auth,
            Err(e) => return ClusterProbeResult::down(e.to_string()),
        };
        let conn_str = crate::externalsvc::postgres_cluster::monitor_connection_string(
            &monitor_host,
            monitor_port,
            auth.as_ref(),
        );

        let start = Instant::now();
        let connect = tokio::time::timeout(
            PROBE_TIMEOUT,
            temps_query_postgres::connect_with_self_signed_tls(&conn_str),
        )
        .await;

        let client = match connect {
            Err(_) => {
                return ClusterProbeResult::down(format!(
                    "Monitor probe to {monitor_host}:{monitor_port} timed out after {}s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(Err(e)) => {
                return ClusterProbeResult::down(format!(
                    "Monitor connect to {monitor_host}:{monitor_port} failed: {}",
                    format_pg_error(&e)
                ));
            }
            Ok(Ok(client)) => client,
        };

        // Read per-data-node reportedstate from the monitor. The monitor's
        // pgautofailover.node table holds one row per registered data node.
        let rows_result = tokio::time::timeout(
            PROBE_TIMEOUT,
            client.query(
                // `health` matters as much as `reportedstate`: when every node
                // dies at once the monitor has nothing to promote, so the FSM
                // leaves the last reported states in place and a dead cluster
                // still reads as `primary`/`secondary`. Only `health` reveals
                // it. (-1 = not yet checked, 0 = failing, 1 = responding.)
                "SELECT nodename::text, nodehost::text, reportedstate::text, health \
                 FROM pgautofailover.node",
                &[],
            ),
        )
        .await;

        // Drop the client to close the connection cleanly. The driver task
        // is owned by `connect_with_self_signed_tls` and exits when the
        // client handle is dropped.
        drop(client);

        let rows = match rows_result {
            Err(_) => {
                return ClusterProbeResult::down(format!(
                    "Monitor query to {monitor_host}:{monitor_port} timed out after {}s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Ok(Err(e)) => {
                return ClusterProbeResult::down(format!(
                    "Monitor query failed at {monitor_host}:{monitor_port}: {}",
                    format_pg_error(&e)
                ));
            }
            Ok(Ok(r)) => r,
        };

        let elapsed_ms = start.elapsed().as_millis();
        let response_time_ms = i32::try_from(elapsed_ms).ok();

        if rows.is_empty() {
            // Monitor reachable but no data nodes registered — cluster is
            // half-built. Treat as Down so it's visibly broken, and say where
            // to look: the monitor is fine, the data members never reached it.
            let data_members = members.iter().filter(|m| !is_role_monitor(&m.role)).count();
            return ClusterProbeResult::down(format!(
                "Monitor at {monitor_host}:{monitor_port} reports zero data nodes: none of the \
                 cluster's {data_members} data member(s) has registered with it. Data members \
                 register by connecting to the monitor, so check their container logs for \
                 pg_autoctl connection errors"
            ));
        }

        let states: Vec<ClusterNodeState> = rows
            .iter()
            .map(|row| ClusterNodeState {
                name: row.get::<_, &str>(0).to_string(),
                state: row.get::<_, &str>(2).to_string(),
                health: row.get::<_, i32>(3),
            })
            .collect();

        let (status, error_message) = classify_cluster_states(service.id, &states);
        ClusterProbeResult {
            status,
            response_time_ms,
            error_message,
        }
    }

    /// Read per-member health for a cluster from the monitor + the current
    /// primary. Used by the UI's Members table — gives one row per data
    /// node with role, reported state, replication sync state, and
    /// replay lag in ms.
    ///
    /// Two queries:
    /// 1. `pgautofailover.node` from the monitor (TLS, autoctl_node) —
    ///    authoritative for `reportedstate` / `candidatepriority` /
    ///    `replicationquorum`.
    /// 2. `pg_stat_replication` from the current primary (credential-safe TLS
    ///    ladder, application user) — gives `sync_state` and `replay_lag` per
    ///    streaming replica, joined to step 1 by `application_name = nodename`.
    ///
    /// Best-effort on (2): if the primary is briefly unreachable mid-failover,
    /// the per-member sync_state/replay_lag fields are left `None` and the
    /// caller can still render the topology view.
    pub async fn cluster_health(&self, service: &external_services::Model) -> ClusterHealthReport {
        use std::time::{Duration, Instant};
        const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

        // ---- locate the monitor ----
        let members = match self.get_service_members(service.id).await {
            Ok(m) => m,
            Err(e) => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Failed to load cluster members for service {}: {}",
                        service.id, e
                    )),
                };
            }
        };
        // Every data member but the primary is meant to be a standby.
        let configured_standbys = members
            .iter()
            .filter(|m| is_role_data_member(&m.role))
            .count()
            .saturating_sub(1);
        let monitor = match members.iter().find(|m| is_role_monitor(&m.role)) {
            Some(m) => m,
            None => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Cluster service {} has no monitor member",
                        service.id
                    )),
                };
            }
        };

        let monitor_host = match control_plane_member_dial_host(
            self.db.as_ref(),
            monitor.node_id,
            &monitor.container_name,
        )
        .await
        {
            Ok(host) => host,
            Err(reason) => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Cannot reach the monitor of cluster service {}: {reason}",
                        service.id
                    )),
                };
            }
        };
        let monitor_port = monitor.port.unwrap_or(5432);

        // SECURITY: keep `sslmode=require`: the self-signed connector may skip
        // certificate authentication, but it must never downgrade this socket
        // to cleartext. `autoctl_node` authenticates with SCRAM.
        let monitor_auth = match crate::externalsvc::postgres_cluster::load_cluster_auth_secrets(
            self.db.as_ref(),
            self.encryption_service.as_ref(),
            service.id,
        )
        .await
        {
            Ok(auth) => auth,
            Err(e) => {
                warn!(
                    service_id = service.id,
                    error = %e,
                    "Could not load cluster monitor credentials; probing without them"
                );
                None
            }
        };
        let monitor_conn_str = crate::externalsvc::postgres_cluster::monitor_connection_string(
            &monitor_host,
            monitor_port,
            monitor_auth.as_ref(),
        );

        let start = Instant::now();
        let monitor_client = match tokio::time::timeout(
            PROBE_TIMEOUT,
            temps_query_postgres::connect_with_self_signed_tls(&monitor_conn_str),
        )
        .await
        {
            Err(_) => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: PROBE_TIMEOUT.as_millis() as i64,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor probe to {monitor_host}:{monitor_port} timed out after {}s",
                        PROBE_TIMEOUT.as_secs()
                    )),
                };
            }
            Ok(Err(e)) => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: 0,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor connect to {monitor_host}:{monitor_port} failed: {}",
                        format_pg_error(&e)
                    )),
                };
            }
            Ok(Ok(client)) => client,
        };

        let nodes_rows = match tokio::time::timeout(
            PROBE_TIMEOUT,
            monitor_client.query(
                "SELECT nodename::text, nodehost::text, nodeport::int4, \
                        reportedstate::text, goalstate::text, \
                        health::int4, \
                        EXTRACT(EPOCH FROM (now() - reporttime))::int8 AS sec_since_report, \
                        candidatepriority::int4, \
                        replicationquorum::bool, \
                        nodeid::int8 \
                 FROM pgautofailover.node",
                &[],
            ),
        )
        .await
        {
            Err(_) => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: start.elapsed().as_millis() as i64,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor query to {monitor_host}:{monitor_port} timed out"
                    )),
                };
            }
            Ok(Err(e)) => {
                return ClusterHealthReport {
                    configured_standbys: 0,
                    checked_at: chrono::Utc::now(),
                    monitor_response_ms: start.elapsed().as_millis() as i64,
                    members: vec![],
                    monitor_error: Some(format!(
                        "Monitor query failed at {monitor_host}:{monitor_port}: {}",
                        format_pg_error(&e)
                    )),
                };
            }
            Ok(Ok(rows)) => rows,
        };
        drop(monitor_client);

        let monitor_response_ms = start.elapsed().as_millis() as i64;

        // Build the per-member view from monitor rows. We'll fill
        // sync_state / replay_lag_ms in the next step from the primary.
        let mut by_name: std::collections::HashMap<String, ClusterMemberHealth> =
            std::collections::HashMap::new();
        let mut primary_member_name: Option<String> = None;
        // (nodeid, nodename, nodehost) — keys for the pg_stat_replication join.
        let mut replication_keys: Vec<(i64, String, String)> = Vec::new();
        for row in &nodes_rows {
            let nodename: String = row.get(0);
            let nodehost: String = row.get(1);
            let nodeport: i32 = row.get(2);
            let reported_state: String = row.get(3);
            let goal_state: String = row.get(4);
            let health: i32 = row.get(5);
            let seconds_since_report: i64 = row.get(6);
            let candidate_priority: i32 = row.get(7);
            let replication_quorum: bool = row.get(8);
            let node_id: i64 = row.get(9);
            replication_keys.push((node_id, nodename.clone(), nodehost.clone()));

            // Only treat a node as primary for the pg_stat_replication
            // join if pg_auto_failover *currently* believes it's primary
            // AND the node is healthy. A stale ghost-primary
            // (`reportedstate='primary'` but `health<=0`) would otherwise
            // route us to a dead host and the panel would lose sync data.
            if live_state_is_writable_primary(Some(&reported_state))
                && health == 1
                && seconds_since_report < 30
            {
                primary_member_name = Some(nodename.clone());
            }

            by_name.insert(
                nodename.clone(),
                ClusterMemberHealth {
                    nodename,
                    nodehost,
                    nodeport,
                    reported_state,
                    goal_state,
                    health,
                    seconds_since_report,
                    candidate_priority,
                    replication_quorum,
                    sync_state: None,
                    replay_lag_ms: None,
                },
            );
        }

        // ---- replication state from the primary, best-effort ----
        //
        // We connect as the cluster's *application* user (whose hba was
        // opened by the node startup script in A1) — `autoctl_node` only
        // has hba access against the monitor's `pg_auto_failover` DB, not
        // the data nodes' `postgres` DB.
        //
        // The join key is `application_name`: pg_auto_failover names each
        // standby's walreceiver `pgautofailover_standby_<nodeid>`, and the
        // monitor rows carry `nodeid`. `client_addr` is only a fallback — it
        // is ambiguous when members share a host and rewritten by NAT (see
        // `replication_row_member`).
        // SECURITY: the monitor decides which persisted member is primary, but
        // never where credentials are sent. Resolve the selected nodename back
        // to the member row and its provisioned node address/port. A forged
        // monitor can therefore lie about state, but cannot redirect the
        // application password to its own `nodehost`/`nodeport`.
        let trusted_primary_endpoint = match primary_member_name
            .as_deref()
            .and_then(|name| trusted_primary_member(&members, name))
        {
            Some(member) => self.stored_member_endpoint(service.id, member).await.ok(),
            None => None,
        };

        if let Some((primary_host, primary_port)) = trusted_primary_endpoint {
            let app_creds = self
                .get_service_parameters(service.id)
                .await
                .ok()
                .map(|params| {
                    let user = params
                        .get("username")
                        .and_then(|v| v.as_str())
                        .unwrap_or("postgres")
                        .to_string();
                    let password = params
                        .get("password")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let database = params
                        .get("database")
                        .and_then(|v| v.as_str())
                        .unwrap_or("postgres")
                        .to_string();
                    (user, password, database)
                });

            if let Some((user, password, database)) = app_creds {
                if let Ok(Ok(primary_client)) = tokio::time::timeout(
                    PROBE_TIMEOUT,
                    temps_query_postgres::connect_with_private_tls_ladder(
                        &primary_host,
                        primary_port,
                        &user,
                        &password,
                        &database,
                    ),
                )
                .await
                {
                    if let Ok(Ok(rep_rows)) = tokio::time::timeout(
                        PROBE_TIMEOUT,
                        primary_client.query(
                            "SELECT host(client_addr)::text AS client_host, \
                                    sync_state::text, \
                                    EXTRACT(EPOCH FROM replay_lag)::float8 * 1000.0 AS replay_lag_ms, \
                                    application_name::text \
                             FROM pg_stat_replication",
                            &[],
                        ),
                    )
                    .await
                    {
                        for row in &rep_rows {
                            let client_host: Option<String> = row.get(0);
                            let sync_state: String = row.get(1);
                            let replay_ms: Option<f64> = row.try_get(2).ok();
                            let application_name: String = row.get(3);
                            if let Some(member_name) = replication_row_member(
                                &application_name,
                                client_host.as_deref(),
                                &replication_keys,
                            ) {
                                if let Some(member) = by_name.get_mut(&member_name) {
                                    member.sync_state = Some(sync_state);
                                    member.replay_lag_ms = replay_ms.map(|v| v as i64);
                                }
                            }
                        }
                    }
                    drop(primary_client);
                }
            }
        }

        // Stable-order output: by candidate_priority desc, then nodename
        // ascending so the UI doesn't reshuffle on every poll.
        let mut members: Vec<ClusterMemberHealth> = by_name.into_values().collect();
        members.sort_by(|a, b| {
            b.candidate_priority
                .cmp(&a.candidate_priority)
                .then(a.nodename.cmp(&b.nodename))
        });

        ClusterHealthReport {
            configured_standbys,
            checked_at: chrono::Utc::now(),
            monitor_response_ms,
            members,
            monitor_error: None,
        }
    }

    /// Run a standalone service's provider-authenticated health probe in the
    /// runtime that owns its container. The control plane retains scheduling,
    /// history, and alerting; only node-local execution crosses the agent API.
    pub async fn probe_service_health(
        &self,
        service: &external_services::Model,
    ) -> Result<crate::externalsvc::HealthProbeResult, ExternalServiceError> {
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service.id,
                service_type: service.service_type.clone(),
            }
        })?;
        let service_config = self.get_service_config(service.id).await?;

        if let ServiceExecutionRoute::Remote(node_id) = service_execution_route(service.node_id) {
            info!(
                service_id = service.id,
                node_id, "Dispatching external-service health probe to owning node"
            );
            let client = self.get_remote_client(node_id).await?;
            return client
                .probe_health(crate::remote_service_client::RemoteHealthProbeRequest {
                    service_config,
                })
                .await
                .map(|response| response.result)
                .map_err(|error| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to probe service {} on node {}: {}",
                        service.id, node_id, error
                    ),
                });
        }

        let service_instance = self.create_service_instance_for_parameter_value(
            service.name.clone(),
            service_type,
            &service_config.parameters,
        )?;
        service_instance
            .health_probe(service_config)
            .await
            .map_err(|error| ExternalServiceError::InternalError {
                reason: format!(
                    "Local health probe failed for service {}: {}",
                    service.id, error
                ),
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::services::test_support::*;

    /// `check_service_health` backed `GET /external-services/{id}/health` with
    /// a hardcoded `false`, so the endpoint called every service unhealthy no
    /// matter what the health monitor had just written to the row.
    #[tokio::test]
    async fn health_check_reports_the_monitors_verdict() {
        for (persisted, expected) in [
            (Some("operational"), true),
            (Some("degraded"), false),
            (Some("down"), false),
            (None, false),
        ] {
            let mut model = encrypted_service_model(1, serde_json::json!({}));
            model.health_status = persisted.map(String::from);
            let manager = mock_service_manager(vec![vec![model]]);

            assert_eq!(
                manager
                    .check_service_health(1)
                    .await
                    .expect("health lookup should succeed"),
                expected,
                "persisted health_status {persisted:?} should report {expected}"
            );
        }
    }
}
