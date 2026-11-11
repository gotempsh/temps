// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-side TLS of managed services (ADR 093): which services support it,
//! the effective status, changing the mode, and handing the certificate to
//! the engine when a container is (re)created.
//!
//! A standalone PostgreSQL container gets the certificate when it is
//! (re)created. A cluster's data nodes get it in place, over member exec
//! (locally or through the agent on a worker), with no restart: see
//! `temps_providers_postgres::postgres_cluster_tls`.

use crate::managed_tls::{
    agreed_member_state, AuthorityCertificate, AuthorityInfo, ContainerTlsState, ManagedTlsService,
    MemberTlsState, ServerTlsError, ServiceTlsStatus,
};
use crate::services::*;
use std::collections::HashMap;
use std::time::Duration;
use temps_entities::external_services;
use temps_providers_core::externalsvc::naming::postgres_container_name;
use temps_providers_core::externalsvc::server_tls::{
    ServerTlsMaterial, ServerTlsMode, SERVER_TLS_FINGERPRINT_LABEL, SERVER_TLS_MATERIAL_PARAM,
    SERVER_TLS_MODE_PARAM,
};
use temps_providers_postgres::postgres_cluster_tls::{
    parse_served_fingerprint, served_certificate_command, server_tls_apply_command,
    ClusterServerCertificate, CLUSTER_TLS_CERT_ENV, CLUSTER_TLS_KEY_ENV,
    MEMBER_APPLY_DEADLINE_SECS, MEMBER_KILL_GRACE_SECS, MEMBER_STATUS_DEADLINE_SECS,
    NOT_INITIALIZED_EXIT_CODE,
};
use tracing::{debug, info, warn};

/// Upper bound for applying the setting on one data node. Longer than the
/// script's own deadline inside the node plus its kill grace
/// ([`MEMBER_APPLY_DEADLINE_SECS`]), so a pass that gives up on a member
/// never releases the service's TLS lock while that script still runs.
const MEMBER_APPLY_TIMEOUT: Duration = Duration::from_secs(120);

// A pass gives up on a member only after its script is dead inside the node.
const _: () =
    assert!(MEMBER_APPLY_TIMEOUT.as_secs() > MEMBER_APPLY_DEADLINE_SECS + MEMBER_KILL_GRACE_SECS);
const _: () = assert!(
    MEMBER_INSPECT_TIMEOUT.as_secs() > MEMBER_STATUS_DEADLINE_SECS + MEMBER_KILL_GRACE_SECS
);

/// A standalone service between being created and running: its creation or
/// start may already have read the TLS mode, so a change recorded now could
/// be missed until the next restart, and re-initializing it would race that
/// start. A TLS change waits until it is running or stopped.
fn standalone_mid_start(status: &str) -> bool {
    matches!(status, "pending" | "creating" | "starting")
}

/// Put a standalone database back on its previous TLS mode after applying
/// a new one failed: record the previous mode again, then run the service's
/// initialisation again so the container serves it (and runs, if the failed
/// attempt had stopped it). Not after the recreate guard's verdict
/// (`reinitialize` false): re-initializing then would recreate over an
/// original that may still be set aside, or start a replacement that failed
/// verification. Returns what happened, for the error, and whether the
/// previous mode is recorded again.
async fn standalone_rollback<R, RE, F, Fut, FE>(
    record_previous: R,
    reinitialize: bool,
    reapply: F,
) -> (String, bool)
where
    R: std::future::Future<Output = Result<ServerTlsMode, RE>>,
    RE: std::fmt::Display,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), FE>>,
    FE: std::fmt::Display,
{
    if let Err(error) = record_previous.await {
        return (
            format!(
                "it could not be recorded again ({error}), so the new mode stays recorded and \
                 the database was left as the failed attempt left it; the TLS status shows \
                 what it serves"
            ),
            false,
        );
    }
    if !reinitialize {
        return (
            "it is recorded, but the database was not re-initialized: the recreate guard \
             stopped the change (below), and its container is left as the guard left it"
                .to_string(),
            true,
        );
    }
    let outcome = match reapply().await {
        Ok(()) => "the database serves it again".to_string(),
        Err(error) => format!(
            "it is recorded, but putting the database back on it failed too ({error}); \
             the TLS status shows what it serves and its next start applies it"
        ),
    };
    (outcome, true)
}

/// After a running standalone was re-initialized for a new TLS mode: the
/// error to return when its container still does not serve that mode. Its
/// start repairs configuration drift only when the recreate guard lets it;
/// a guard that refuses, or rolls back to the original, leaves the original
/// running unchanged, and the change must not be reported as done. The
/// previous mode, which that container still serves, is recorded again.
async fn standalone_not_applied<R, RE>(
    service: &external_services::Model,
    mode: ServerTlsMode,
    previous: ServerTlsMode,
    served: &ServiceTlsStatus,
    record_previous: R,
) -> Option<ServerTlsError>
where
    R: std::future::Future<Output = Result<ServerTlsMode, RE>>,
    RE: std::fmt::Display,
{
    if !served.pending_restart() {
        return None;
    }
    let (rollback, previous_recorded) = match record_previous.await {
        Ok(_) => ("the previous mode is recorded again".to_string(), true),
        Err(error) => (
            format!(
                "recording the previous mode again failed ({error}), so the new mode stays \
                 recorded and the TLS status shows the setting ahead of the container until \
                 its next start"
            ),
            false,
        ),
    };
    Some(ServerTlsError::NotApplied {
        service_id: service.id,
        name: service.name.clone(),
        mode,
        previous,
        rollback,
        previous_recorded,
    })
}

/// When a pass may treat a failed apply on a member as finished. A local
/// exec whose output stream failed returns at once while its script may
/// still run (Docker never kills it), so the pass holds the service's TLS
/// lock until that script's own deadline and kill grace have passed: a
/// rollback or the next change must not race it on the same certificate
/// files and settings. A worker's agent already holds each container until
/// its exec has stopped, so a remote failure is final when it returns.
fn member_apply_fence(
    started: tokio::time::Instant,
    member_is_local: bool,
) -> Option<tokio::time::Instant> {
    member_is_local
        .then(|| started + Duration::from_secs(MEMBER_APPLY_DEADLINE_SECS + MEMBER_KILL_GRACE_SECS))
}

/// Upper bound for reading the certificate one data node serves (the script
/// dies at [`MEMBER_STATUS_DEADLINE_SECS`] inside the node).
const MEMBER_INSPECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How often, and for how long, a cluster whose data nodes were still being
/// created (or failed) is converged again in the background after creation,
/// a new member or a start: a standby's first base backup can take minutes.
const FOLLOW_UP_INTERVAL: Duration = Duration::from_secs(15);
const FOLLOW_UP_ATTEMPTS: u32 = 40;

/// Why `service` cannot use managed server TLS yet, or `None` when it can.
///
/// Phase 1 covers standalone PostgreSQL that Temps runs on the control
/// plane, and PostgreSQL HA clusters on any nodes. Everything else is
/// refused with the reason, never silently ignored.
pub(crate) fn server_tls_unsupported_reason(
    service: &external_services::Model,
    parameters: &HashMap<String, serde_json::Value>,
) -> Option<String> {
    if service.service_type != "postgres" {
        return Some(format!(
            "managed server TLS is available for PostgreSQL first; {} follows in a later release",
            service.service_type
        ));
    }
    if service.topology == "cluster" {
        // Applied to each data node in place, wherever it runs.
        return None;
    }
    if service.node_id.is_some() {
        return Some("services on worker nodes are not supported yet".to_string());
    }
    if parameters
        .get("container_name")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.is_empty())
    {
        return Some(
            "the service was imported from an existing container, whose configuration Temps \
             does not manage"
                .to_string(),
        );
    }
    None
}

/// Whether a new service gets `available` at creation: every new PostgreSQL
/// cluster, and every new standalone PostgreSQL service on the control plane
/// (ADR 093). Existing services, and anything else, stay `off` (no row).
pub(crate) fn default_tls_mode_for_new_service(
    service_type: ServiceType,
    topology: &str,
    node_id: Option<i32>,
    parameters: &HashMap<String, serde_json::Value>,
) -> Option<ServerTlsMode> {
    let eligible = matches!(service_type, ServiceType::Postgres)
        && (topology == "cluster"
            || (node_id.is_none() && !parameters.contains_key("container_name")));
    eligible.then_some(ServerTlsMode::Available)
}

/// Record the TLS mode a service is created with, in its creation
/// transaction. `None` records nothing: no row means `off`.
pub(super) async fn record_initial_tls_mode<C: sea_orm::ConnectionTrait>(
    txn: &C,
    service_id: i32,
    mode: Option<ServerTlsMode>,
) -> Result<(), ExternalServiceError> {
    let Some(mode) = mode else {
        return Ok(());
    };
    crate::managed_tls::set_mode_on(txn, service_id, mode)
        .await
        .map(|_previous| ())
        .map_err(|e| ExternalServiceError::InternalError {
            reason: format!(
                "Failed to record server TLS mode '{mode}' for new service {service_id}: {e}"
            ),
        })
}

/// The names a managed PostgreSQL service is reached at: its container name
/// on the `temps` network, its overlay name, and loopback for host clients
/// (the published port binds 127.0.0.1 only).
pub(crate) fn postgres_subject_alt_names(service: &external_services::Model) -> Vec<String> {
    let mut names = vec![postgres_container_name(&service.name)];
    if let Some(fqdn) =
        crate::service_dns::standalone_service_fqdn(&service.name, service.slug.as_deref())
    {
        names.push(fqdn);
    }
    names.push("localhost".to_string());
    names.push("127.0.0.1".to_string());
    names
}

/// The names a cluster's data nodes are reached at: the cluster VIP apps
/// receive (`<name>.temps.local`), and for every data node its container
/// name, advertised hostname and overlay address, plus loopback. One
/// certificate covers every data node: they hold the same data and trust
/// each other already, and whichever is primary must match the VIP.
pub(crate) fn cluster_subject_alt_names(
    service: &external_services::Model,
    members: &[ServiceMemberInfo],
) -> Vec<String> {
    let mut names = vec![format!("{}.temps.local", service.name)];
    if let Some(fqdn) =
        crate::service_dns::standalone_service_fqdn(&service.name, service.slug.as_deref())
    {
        names.push(fqdn);
    }
    for member in members.iter().filter(|m| !is_role_monitor(&m.role)) {
        names.push(member.container_name.clone());
        names.extend(member.hostname.iter().cloned());
        names.extend(member.compute_ip.iter().cloned());
    }
    names.push("localhost".to_string());
    names.push("127.0.0.1".to_string());
    temps_core::managed_service_pki::normalize_subject_alt_names(&names)
}

/// What applying a cluster's setting did on each data node.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ClusterTlsApplyReport {
    /// Data nodes now serving the target certificate.
    pub applied: Vec<String>,
    /// `(member, why)` for data nodes left alone because they are not
    /// running. The next start applies it.
    pub skipped: Vec<(String, String)>,
    /// Data nodes still being created (`pg_autoctl create`, a standby's
    /// first base backup): applied by the follow-up once they are up.
    pub initializing: Vec<String>,
    /// `(member, reason)` for data nodes where applying failed.
    pub failed: Vec<(String, String)>,
}

impl ClusterTlsApplyReport {
    /// Every data node that can take the setting now has it.
    fn converged(&self) -> bool {
        self.failed.is_empty() && self.initializing.is_empty()
    }

    fn describe_failures(&self) -> String {
        self.failed
            .iter()
            .map(|(member, reason)| format!("{member}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// TLS state of a container from its inspected CMD and labels.
pub(crate) fn container_tls_state(
    cmd: &[String],
    labels: Option<&HashMap<String, String>>,
) -> ContainerTlsState {
    let ssl_on = cmd
        .iter()
        .any(|token| token.trim().eq_ignore_ascii_case("ssl=on"));
    if !ssl_on {
        return ContainerTlsState::Off;
    }
    ContainerTlsState::On {
        fingerprint_sha256: labels
            .and_then(|labels| labels.get(SERVER_TLS_FINGERPRINT_LABEL))
            .cloned(),
    }
}

impl ExternalServiceManager {
    fn managed_tls(&self) -> ManagedTlsService {
        ManagedTlsService::new(self.db.clone(), self.encryption_service.clone())
    }

    /// The `ServiceConfig` parameters that tell the engine its TLS mode and
    /// give it the certificate to install. Empty for a service that does not
    /// support managed TLS, so it keeps exactly today's behaviour.
    ///
    /// Only container lifecycle paths call this: the key is decrypted into
    /// memory for the engine that writes it into the service's volume, and
    /// never added to the parameters other consumers read.
    pub(super) async fn server_tls_parameters(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<(String, serde_json::Value)>, ExternalServiceError> {
        // A cluster's data nodes get the certificate in place
        // (`apply_cluster_server_tls`), never through container parameters.
        if service.topology == "cluster"
            || server_tls_unsupported_reason(service, parameters).is_some()
        {
            return Ok(Vec::new());
        }
        let tls_error =
            |e: crate::managed_tls::ManagedTlsError| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to prepare server TLS for service {} ('{}'): {}",
                    service.id, service.name, e
                ),
            };
        let tls = self.managed_tls();
        let record = tls.record(service.id).await.map_err(tls_error)?;
        let mut out = vec![(
            SERVER_TLS_MODE_PARAM.to_string(),
            serde_json::Value::String(record.mode.as_str().to_string()),
        )];
        if record.mode.offers_tls() {
            let material = tls
                .ensure_server_certificate(
                    service.id,
                    &postgres_container_name(&service.name),
                    &postgres_subject_alt_names(service),
                )
                .await
                .map_err(tls_error)?;
            let value = serde_json::to_value(&material).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to encode the server TLS material of service {}: {}",
                        service.id, e
                    ),
                }
            })?;
            out.push((SERVER_TLS_MATERIAL_PARAM.to_string(), value));
        }
        Ok(out)
    }

    /// The certificate a standalone PostgreSQL container of `service` must
    /// serve, for container builders outside the engine's lifecycle (the
    /// major-upgrade adapter). `None` when its mode does not offer TLS or it
    /// cannot use managed TLS (a cluster, an imported container).
    pub(crate) async fn standalone_server_tls_material(
        &self,
        service: &external_services::Model,
    ) -> Result<Option<ServerTlsMaterial>, ExternalServiceError> {
        let parameters = self.get_service_parameters(service.id).await?;
        let material = self
            .server_tls_parameters(service, &parameters)
            .await?
            .into_iter()
            .find(|(key, _)| key == SERVER_TLS_MATERIAL_PARAM)
            .map(|(_, value)| serde_json::from_value::<ServerTlsMaterial>(value))
            .transpose()
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!(
                    "Failed to decode the server TLS material of service {} ('{}'): {}",
                    service.id, service.name, e
                ),
            })?;
        Ok(material)
    }

    /// The effective server TLS status of a service.
    pub async fn get_service_tls_status(
        &self,
        service_id: i32,
    ) -> Result<ServiceTlsStatus, ServerTlsError> {
        let service = self.get_service(service_id).await.map_err(|e| match e {
            ExternalServiceError::ServiceNotFound { .. } => ServerTlsError::NotFound { service_id },
            other => ServerTlsError::Service(other),
        })?;
        let parameters = self.get_service_parameters(service_id).await?;
        self.tls_status_for(&service, &parameters).await
    }

    async fn tls_status_for(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<ServiceTlsStatus, ServerTlsError> {
        let tls = self.managed_tls();
        let record = tls.record(service.id).await?;
        let authority = tls.authority().await?.as_ref().map(AuthorityInfo::from);
        let unsupported_reason = server_tls_unsupported_reason(service, parameters);
        let cluster = service.topology == "cluster";
        let mut members = Vec::new();
        let container = if unsupported_reason.is_some() {
            ContainerTlsState::Unknown
        } else if cluster {
            let issued = record
                .certificate
                .as_ref()
                .map(|c| c.fingerprint_sha256.clone());
            members = self
                .inspect_cluster_member_tls(service.id, issued.as_deref())
                .await?;
            agreed_member_state(&members)
        } else {
            self.inspect_container_tls(&postgres_container_name(&service.name))
                .await
        };
        Ok(ServiceTlsStatus {
            service_id: service.id,
            mode: record.mode,
            unsupported_reason,
            certificate: record.certificate,
            authority,
            container,
            cluster,
            members,
        })
    }

    /// The certificate every data node of a cluster serves, read
    /// concurrently and each bounded by [`MEMBER_INSPECT_TIMEOUT`]. A member
    /// that is not running, or cannot be reached, is `Unknown` with why.
    async fn inspect_cluster_member_tls(
        &self,
        service_id: i32,
        issued: Option<&str>,
    ) -> Result<Vec<MemberTlsState>, ServerTlsError> {
        let members = self.get_service_members(service_id).await?;
        let data_nodes: Vec<&ServiceMemberInfo> = members
            .iter()
            .filter(|m| !is_role_monitor(&m.role))
            .collect();
        let reads = data_nodes.iter().map(|member| async move {
            let unknown = |detail: String| MemberTlsState {
                container_name: member.container_name.clone(),
                status: member.status.clone(),
                tls: ContainerTlsState::Unknown,
                detail: Some(detail),
            };
            if member.status != "running" {
                return unknown(format!("the member is {}", member.status));
            }
            let exec = self.exec_in_member_with_env(
                member,
                served_certificate_command(),
                None,
                HashMap::new(),
            );
            match tokio::time::timeout(MEMBER_INSPECT_TIMEOUT, exec).await {
                Err(_) => unknown(format!(
                    "no answer within {}s",
                    MEMBER_INSPECT_TIMEOUT.as_secs()
                )),
                Ok(Err(error)) => unknown(error.to_string()),
                Ok(Ok((code, stdout, stderr))) => match parse_served_fingerprint(&stdout) {
                    Some(served) if code == 0 => MemberTlsState {
                        container_name: member.container_name.clone(),
                        status: member.status.clone(),
                        tls: ContainerTlsState::from_served_fingerprint(&served, issued),
                        detail: None,
                    },
                    _ => unknown(format!(
                        "reading the served certificate exited {code}: {}",
                        stderr.trim()
                    )),
                },
            }
        });
        Ok(futures::future::join_all(reads).await)
    }

    /// Make every running data node of cluster `service` serve what its
    /// setting asks for: the managed certificate (issued, or re-issued when
    /// the data nodes' names changed or it nears expiry) when TLS is
    /// offered, pg_auto_failover's self-signed one otherwise. Members are
    /// done one at a time and a failure on one does not stop the others; the
    /// report says what happened on each. A cluster whose setting was never
    /// on has nothing to undo and is left alone.
    pub(crate) async fn apply_cluster_server_tls(
        &self,
        service: &external_services::Model,
    ) -> Result<ClusterTlsApplyReport, ServerTlsError> {
        // Serialize per service and read the setting under the lock, so an
        // older pass can never land after a newer one.
        let lock = self.service_tls_locks.for_service(service.id).await;
        let held = lock.lock().await;
        self.apply_cluster_server_tls_holding(service, &held).await
    }

    /// [`Self::apply_cluster_server_tls`] for a caller that already holds
    /// the service's TLS lock (a mode change, through to its rollback).
    async fn apply_cluster_server_tls_holding(
        &self,
        service: &external_services::Model,
        _held: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<ClusterTlsApplyReport, ServerTlsError> {
        let tls = self.managed_tls();
        let record = tls.record(service.id).await?;
        let members = self.get_service_members(service.id).await?;
        let (target, material): (ClusterServerCertificate, Option<ServerTlsMaterial>) =
            if record.mode.offers_tls() {
                let material = tls
                    .ensure_server_certificate(
                        service.id,
                        &postgres_container_name(&service.name),
                        &cluster_subject_alt_names(service, &members),
                    )
                    .await?;
                (ClusterServerCertificate::Managed, Some(material))
            } else if record.certificate.is_some() {
                (ClusterServerCertificate::SelfSigned, None)
            } else {
                debug!(
                    service_id = service.id,
                    "Cluster never had managed server TLS; its data nodes keep their certificates"
                );
                return Ok(ClusterTlsApplyReport::default());
            };
        let env: HashMap<String, String> = material
            .map(|m| {
                HashMap::from([
                    (CLUSTER_TLS_CERT_ENV.to_string(), m.cert_pem),
                    (CLUSTER_TLS_KEY_ENV.to_string(), m.key_pem),
                ])
            })
            .unwrap_or_default();

        let mut report = ClusterTlsApplyReport::default();
        for member in members.iter().filter(|m| !is_role_monitor(&m.role)) {
            let name = member.container_name.clone();
            if member.status != "running" {
                report
                    .skipped
                    .push((name, format!("the member is {}", member.status)));
                continue;
            }
            let started = tokio::time::Instant::now();
            let exec = self.exec_in_member_with_env(
                member,
                server_tls_apply_command(target),
                None,
                env.clone(),
            );
            match tokio::time::timeout(MEMBER_APPLY_TIMEOUT, exec).await {
                Err(_) => report.failed.push((
                    name,
                    format!("no result within {}s", MEMBER_APPLY_TIMEOUT.as_secs()),
                )),
                Ok(Err(error)) => {
                    if let Some(fence) = member_apply_fence(started, member.node_id.is_none()) {
                        warn!(
                            service_id = service.id,
                            member = %name,
                            error = %error,
                            "TLS apply on a data node failed before it reported a result; \
                             waiting for its script's deadline before another TLS change"
                        );
                        tokio::time::sleep_until(fence).await;
                    }
                    report.failed.push((name, error.to_string()))
                }
                Ok(Ok((0, _, _))) => report.applied.push(name),
                Ok(Ok((NOT_INITIALIZED_EXIT_CODE, _, _))) => report.initializing.push(name),
                Ok(Ok((code, _, stderr))) => report
                    .failed
                    .push((name, format!("exit {code}: {}", stderr.trim()))),
            }
        }
        Ok(report)
    }

    /// Converge a cluster's data nodes on its TLS setting after `trigger`
    /// (start, creation, a new member). Never fails the caller: when a data
    /// node is still being created or applying failed, a background
    /// follow-up retries every [`FOLLOW_UP_INTERVAL`] for a bounded time.
    /// What still does not converge is logged, shown as pending in the TLS
    /// status, and retried on the next start.
    pub(super) async fn reconcile_cluster_server_tls(
        &self,
        service_id: i32,
        trigger: &'static str,
    ) {
        // On a task of its own, like a mode change: a caller dropped
        // mid-pass must not release the TLS lock while a data node's exec
        // still runs.
        // The follow-up is scheduled from that task too, so a dropped
        // caller does not lose the retry either.
        let manager = self.clone();
        let pass = tokio::spawn(async move {
            if !manager
                .converge_cluster_server_tls(service_id, trigger)
                .await
            {
                manager.spawn_cluster_tls_follow_up(service_id, trigger);
            }
        });
        if let Err(error) = pass.await {
            warn!(service_id, trigger, error = %error, "Cluster server TLS pass did not finish");
            self.spawn_cluster_tls_follow_up(service_id, trigger);
        }
    }

    /// [`Self::reconcile_cluster_server_tls`] for each of `service_ids`, one
    /// after another on a task of its own, for callers that must not wait.
    /// Returns the task, which callers may drop.
    pub(super) fn spawn_cluster_server_tls_reconcile(
        &self,
        service_ids: Vec<i32>,
        trigger: &'static str,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            for service_id in service_ids {
                manager
                    .reconcile_cluster_server_tls(service_id, trigger)
                    .await;
            }
        })
    }

    /// The bounded background retry of [`Self::reconcile_cluster_server_tls`].
    /// Each pass takes the service's TLS lock, so it waits for a mode change
    /// in progress and then reads the setting that change left.
    fn spawn_cluster_tls_follow_up(&self, service_id: i32, trigger: &'static str) {
        let manager = self.clone();
        tokio::spawn(async move {
            for attempt in 1..=FOLLOW_UP_ATTEMPTS {
                tokio::time::sleep(FOLLOW_UP_INTERVAL).await;
                if manager
                    .converge_cluster_server_tls(service_id, trigger)
                    .await
                {
                    debug!(
                        service_id,
                        trigger, attempt, "Cluster server TLS converged in the follow-up"
                    );
                    return;
                }
            }
            warn!(
                service_id,
                trigger,
                attempts = FOLLOW_UP_ATTEMPTS,
                "Cluster server TLS did not converge on every data node; the next start retries"
            );
        });
    }

    /// One convergence pass. `true` when nothing is left to retry now: the
    /// setting is applied on every data node that can take it, or the
    /// cluster is gone or not a PostgreSQL cluster.
    async fn converge_cluster_server_tls(&self, service_id: i32, trigger: &str) -> bool {
        let service = match self.get_service(service_id).await {
            Ok(service) => service,
            Err(ExternalServiceError::ServiceNotFound { .. }) => return true,
            Err(error) => {
                warn!(service_id, trigger, error = %error, "Could not load cluster to apply server TLS");
                return false;
            }
        };
        if service.service_type != "postgres" || service.topology != "cluster" {
            return true;
        }
        match self.apply_cluster_server_tls(&service).await {
            Ok(report) => {
                if report.failed.is_empty() {
                    if !report.applied.is_empty() || !report.skipped.is_empty() {
                        info!(
                            service_id,
                            trigger,
                            applied = ?report.applied,
                            skipped = ?report.skipped,
                            initializing = ?report.initializing,
                            "Applied server TLS setting to cluster data nodes"
                        );
                    }
                } else {
                    warn!(
                        service_id,
                        trigger,
                        applied = ?report.applied,
                        initializing = ?report.initializing,
                        failures = %report.describe_failures(),
                        "Server TLS setting not applied to every cluster data node"
                    );
                }
                report.converged()
            }
            Err(error) => {
                warn!(
                    service_id,
                    trigger,
                    error = %error,
                    "Could not apply the server TLS setting to the cluster"
                );
                false
            }
        }
    }

    async fn inspect_container_tls(&self, container: &str) -> ContainerTlsState {
        let Ok(docker) = self.require_docker() else {
            return ContainerTlsState::Unknown;
        };
        match docker
            .inspect_container(
                container,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(info) => {
                let config = info.config.unwrap_or_default();
                container_tls_state(
                    config.cmd.as_deref().unwrap_or_default(),
                    config.labels.as_ref(),
                )
            }
            Err(_) => ContainerTlsState::Unknown,
        }
    }

    /// The managed-services CA certificate (public material only).
    pub async fn get_managed_ca_certificate(
        &self,
        service_id: i32,
    ) -> Result<AuthorityCertificate, ServerTlsError> {
        self.get_service(service_id).await.map_err(|e| match e {
            ExternalServiceError::ServiceNotFound { .. } => ServerTlsError::NotFound { service_id },
            other => ServerTlsError::Service(other),
        })?;
        let authority = self
            .managed_tls()
            .authority()
            .await?
            .ok_or(ServerTlsError::NoAuthority { service_id })?;
        Ok(AuthorityCertificate {
            info: AuthorityInfo::from(&authority),
            certificate_pem: authority.certificate_pem,
        })
    }

    /// Change a service's server TLS mode and apply it.
    ///
    /// A running standalone service is re-initialized through the same path
    /// a parameter update takes (stop, init, start with drift repair), which
    /// recreates the container through the recreate guard: that restarts
    /// the database. A running cluster's data nodes switch certificates in
    /// place, with no restart. A service that is not running only records
    /// the mode; its next start applies it. If applying fails, the previous
    /// mode is restored so the record keeps describing what runs. Returns the
    /// previous mode and the resulting status.
    ///
    /// The change runs on a task of its own, which this future only awaits:
    /// a request dropped mid-change (a client that hangs up) must neither
    /// release the service's TLS lock while a data node's exec, which
    /// Docker never kills, still runs, nor stop a standalone change between
    /// its recreate and its rollback.
    pub async fn set_service_tls_mode(
        &self,
        service_id: i32,
        mode: ServerTlsMode,
    ) -> Result<(ServerTlsMode, ServiceTlsStatus), ServerTlsError> {
        let manager = self.clone();
        tokio::spawn(async move { manager.change_service_tls_mode(service_id, mode).await })
            .await
            .map_err(|error| ServerTlsError::ChangeAborted {
                service_id,
                mode,
                reason: error.to_string(),
            })?
    }

    async fn change_service_tls_mode(
        &self,
        service_id: i32,
        mode: ServerTlsMode,
    ) -> Result<(ServerTlsMode, ServiceTlsStatus), ServerTlsError> {
        // One change (with its rollback) at a time per service, and never
        // while an apply pass runs: the mode read, written, applied and
        // restored below is always the latest one.
        let lock = self.service_tls_locks.for_service(service_id).await;
        let held = lock.lock().await;
        let service = self.get_service(service_id).await.map_err(|e| match e {
            ExternalServiceError::ServiceNotFound { .. } => ServerTlsError::NotFound { service_id },
            other => ServerTlsError::Service(other),
        })?;
        if mode == ServerTlsMode::Required {
            return Err(ServerTlsError::ModeNotYetSupported { service_id, mode });
        }
        let parameters = self.get_service_parameters(service_id).await?;
        if mode.offers_tls() {
            if let Some(reason) = server_tls_unsupported_reason(&service, &parameters) {
                return Err(ServerTlsError::Unsupported {
                    service_id,
                    name: service.name.clone(),
                    reason,
                });
            }
        }
        self.ensure_no_active_upgrade(service_id).await?;

        let tls = self.managed_tls();
        let current = self.tls_status_for(&service, &parameters).await?;
        if current.mode == mode && !current.pending_restart() {
            return Ok((mode, current));
        }
        if service.topology != "cluster" && standalone_mid_start(&service.status) {
            return Err(ServerTlsError::Busy {
                service_id,
                name: service.name.clone(),
                status: service.status.clone(),
            });
        }
        let previous = tls.set_mode(service_id, mode).await?;
        if service.topology == "cluster" {
            if service.status == "running" {
                self.apply_cluster_mode_change(&service, mode, previous, &held)
                    .await?;
            }
        } else if service.status == "running" {
            // Only a running service is re-initialized. One still being
            // created, starting or stopped only records the mode, which its
            // initialization or next start applies: initializing it here
            // would race the creation or start in progress.
            if let Err(error) = self.initialize_service_holding(service_id, &held).await {
                warn!(
                    service_id,
                    mode = %mode,
                    previous = %previous,
                    error = %error,
                    "Applying the server TLS mode failed; restoring the previous mode"
                );
                // The failure may come after the container was already
                // replaced (a later status write), or after the original was
                // stopped: put the database itself back, not only the row.
                let (rollback, previous_recorded) = standalone_rollback(
                    tls.set_mode(service_id, previous),
                    !Self::is_recreate_guard_verdict(&error),
                    || self.initialize_service_holding(service_id, &held),
                )
                .await;
                warn!(service_id, previous = %previous, rollback = %rollback, "Server TLS mode rollback");
                return Err(ServerTlsError::Apply {
                    service_id,
                    mode,
                    previous,
                    rollback,
                    previous_recorded,
                    source: Box::new(error),
                });
            }
            let served = self
                .tls_status_for(&service, &parameters)
                .await
                .map_err(|error| status_unavailable(service_id, mode, previous, error))?;
            if let Some(error) = standalone_not_applied(
                &service,
                mode,
                previous,
                &served,
                tls.set_mode(service_id, previous),
            )
            .await
            {
                warn!(service_id, mode = %mode, previous = %previous, error = %error, "Server TLS mode not applied");
                return Err(error);
            }
            info!(service_id, previous = %previous, mode = %mode, "Server TLS mode of external service changed");
            return Ok((previous, served));
        }
        info!(
            service_id,
            previous = %previous,
            mode = %mode,
            "Server TLS mode of external service changed"
        );
        let status = self
            .tls_status_for(&service, &parameters)
            .await
            .map_err(|error| status_unavailable(service_id, mode, previous, error))?;
        Ok((previous, status))
    }

    /// Apply a just-recorded mode to a running cluster. On any failure the
    /// previous mode is recorded again and re-applied to the data nodes that
    /// had switched (best effort), and the failure is returned.
    async fn apply_cluster_mode_change(
        &self,
        service: &external_services::Model,
        mode: ServerTlsMode,
        previous: ServerTlsMode,
        held: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<(), ServerTlsError> {
        let failures = match self.apply_cluster_server_tls_holding(service, held).await {
            Ok(report) if report.failed.is_empty() => {
                if !report.skipped.is_empty() {
                    info!(
                        service_id = service.id,
                        mode = %mode,
                        skipped = ?report.skipped,
                        "Server TLS mode recorded; data nodes that are not running get it at the next start"
                    );
                }
                if !report.initializing.is_empty() {
                    // Runs once this change releases the lock.
                    self.spawn_cluster_tls_follow_up(service.id, "TLS mode change");
                }
                return Ok(());
            }
            Ok(report) => report.describe_failures(),
            Err(error) => error.to_string(),
        };
        warn!(
            service_id = service.id,
            mode = %mode,
            previous = %previous,
            failures = %failures,
            "Applying the server TLS mode to the cluster failed; restoring the previous mode"
        );
        let tls = self.managed_tls();
        let (rollback, previous_recorded) = match tls.set_mode(service.id, previous).await {
            Ok(_) => {
                let rollback = match self.apply_cluster_server_tls_holding(service, held).await {
                    Ok(report) if report.failed.is_empty() => {
                        format!("the previous mode '{previous}' is restored")
                    }
                    Ok(report) => {
                        let failures = report.describe_failures();
                        warn!(
                            service_id = service.id,
                            failures = %failures,
                            "Some data nodes could not be put back on the previous server TLS mode; \
                             the TLS status lists them and the next start retries"
                        );
                        format!(
                            "the previous mode '{previous}' is recorded again, but putting some \
                             data nodes back on it failed ({failures}); the TLS status lists them \
                             and their next start applies it"
                        )
                    }
                    Err(error) => {
                        warn!(
                            service_id = service.id,
                            error = %error,
                            "Could not put the cluster back on the previous server TLS mode"
                        );
                        format!(
                            "the previous mode '{previous}' is recorded again, but putting the \
                             cluster back on it failed ({error}); the TLS status shows what each \
                             data node serves"
                        )
                    }
                };
                (rollback, true)
            }
            Err(error) => {
                warn!(
                    service_id = service.id,
                    error = %error,
                    "Failed to restore the previous server TLS mode"
                );
                (
                    format!(
                        "recording the previous mode '{previous}' again failed ({error}), so \
                         '{mode}' stays recorded and the TLS status shows what each data node \
                         serves"
                    ),
                    false,
                )
            }
        };
        Err(ServerTlsError::ClusterApply {
            service_id: service.id,
            mode,
            previous,
            failures,
            rollback,
            previous_recorded,
        })
    }
}

/// A change whose new mode is already recorded, and possibly applied, but
/// whose resulting status could not be read: still a change, so it is
/// audited, but not one to report as done.
fn status_unavailable(
    service_id: i32,
    mode: ServerTlsMode,
    previous: ServerTlsMode,
    error: ServerTlsError,
) -> ServerTlsError {
    ServerTlsError::StatusUnavailable {
        service_id,
        mode,
        previous,
        source: Box::new(error),
    }
}

/// One async lock per service, held by a TLS mode change from reading the
/// current mode through its rollback, and by every pass that switches a
/// cluster's data nodes, so they run one at a time and none acts on a stale
/// setting. One small entry per service whose TLS was changed or applied.
#[derive(Debug, Default)]
pub(crate) struct ServiceTlsLocks {
    locks: tokio::sync::Mutex<HashMap<i32, Arc<tokio::sync::Mutex<()>>>>,
}

impl ServiceTlsLocks {
    /// The lock for one service; the same lock every time for the same id.
    pub(crate) async fn for_service(&self, service_id: i32) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .await
            .entry(service_id)
            .or_default()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_tls::IssuedCertificateInfo;
    use chrono::Utc;

    fn service(service_type: &str) -> external_services::Model {
        let mut model = crate::externalsvc::test_utils::create_mock_external_service(
            "orders".to_string(),
            service_type,
            "18",
        );
        model.id = 7;
        model.slug = Some("orders".into());
        model
    }

    #[tokio::test]
    async fn a_failed_standalone_change_puts_the_database_back() {
        let reapplied = std::sync::atomic::AtomicBool::new(false);
        let outcome = standalone_rollback(
            async { Ok::<_, String>(ServerTlsMode::Available) },
            true,
            || async {
                reapplied.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(())
            },
        )
        .await;
        assert!(reapplied.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(outcome, ("the database serves it again".to_string(), true));

        let outcome = standalone_rollback(
            async { Ok::<_, String>(ServerTlsMode::Available) },
            true,
            || async { Err::<(), _>("container 'postgres-orders' did not start".to_string()) },
        )
        .await;
        assert!(outcome
            .0
            .contains("putting the database back on it failed too"));
        assert!(outcome
            .0
            .contains("container 'postgres-orders' did not start"));
        assert!(outcome.1, "the previous mode is recorded again");

        // Without the previous mode recorded, re-initialising would apply the
        // failed one again: it is not attempted.
        let reapplied = std::sync::atomic::AtomicBool::new(false);
        let outcome = standalone_rollback(
            async { Err::<ServerTlsMode, _>("connection reset".to_string()) },
            true,
            || async {
                reapplied.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(())
            },
        )
        .await;
        assert!(!reapplied.load(std::sync::atomic::Ordering::SeqCst));
        assert!(outcome
            .0
            .contains("could not be recorded again (connection reset)"));
        // The new mode stays recorded: the change must still be audited.
        assert!(!outcome.1, "the previous mode is not recorded again");

        // After the recreate guard's verdict the database is never
        // re-initialized, though the previous mode is recorded again.
        let recorded = std::sync::atomic::AtomicBool::new(false);
        let reapplied = std::sync::atomic::AtomicBool::new(false);
        let outcome = standalone_rollback(
            async {
                recorded.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(ServerTlsMode::Available)
            },
            false,
            || async {
                reapplied.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(())
            },
        )
        .await;
        assert!(recorded.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!reapplied.load(std::sync::atomic::Ordering::SeqCst));
        assert!(outcome.0.contains("not re-initialized"));
        assert!(outcome.1);
    }

    /// A start that kept the original container (the recreate guard refused
    /// or rolled back the drift repair) does not count as applying the mode.
    #[tokio::test]
    async fn a_standalone_change_the_guard_kept_out_is_not_reported_as_done() {
        let service = service("postgres");
        let on = ContainerTlsState::On {
            fingerprint_sha256: Some("a".into()),
        };

        // The container serves the new mode: nothing recorded, no error.
        let recorded = std::sync::atomic::AtomicBool::new(false);
        let outcome = standalone_not_applied(
            &service,
            ServerTlsMode::Available,
            ServerTlsMode::Off,
            &status(ServerTlsMode::Available, on.clone(), Some("a")),
            async {
                recorded.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(ServerTlsMode::Available)
            },
        )
        .await;
        assert!(outcome.is_none());
        assert!(!recorded.load(std::sync::atomic::Ordering::SeqCst));

        // Still plaintext after the start: the previous mode is recorded
        // again and the change fails with what the database still serves.
        let outcome = standalone_not_applied(
            &service,
            ServerTlsMode::Available,
            ServerTlsMode::Off,
            &status(ServerTlsMode::Available, ContainerTlsState::Off, None),
            async {
                recorded.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, String>(ServerTlsMode::Available)
            },
        )
        .await
        .expect("an unapplied mode is an error");
        assert!(recorded.load(std::sync::atomic::Ordering::SeqCst));
        let message = outcome.to_string();
        assert!(
            matches!(
                outcome,
                ServerTlsError::NotApplied {
                    service_id: 7,
                    mode: ServerTlsMode::Available,
                    previous: ServerTlsMode::Off,
                    ..
                }
            ),
            "{message}"
        );
        assert!(message.contains("still serves 'off'"), "{message}");
        assert!(message.contains("recorded again"), "{message}");
        assert_eq!(outcome.recorded_change(), None, "the previous mode is back");

        let outcome = standalone_not_applied(
            &service,
            ServerTlsMode::Off,
            ServerTlsMode::Available,
            &status(ServerTlsMode::Off, on, Some("a")),
            async { Err::<ServerTlsMode, _>("connection reset".to_string()) },
        )
        .await
        .expect("an unapplied mode is an error");
        assert!(outcome.to_string().contains("failed (connection reset)"));
        // The new mode stays recorded, so the failed change is still audited.
        assert_eq!(
            outcome.recorded_change(),
            Some((ServerTlsMode::Available, ServerTlsMode::Off))
        );
    }

    #[test]
    fn a_standalone_change_waits_for_a_start_in_progress() {
        for status in ["pending", "creating", "starting"] {
            assert!(standalone_mid_start(status), "{status}");
        }
        for status in ["running", "stopped", "failed"] {
            assert!(!standalone_mid_start(status), "{status}");
        }
    }

    #[test]
    fn a_failed_local_apply_is_final_only_once_its_script_is_dead() {
        let started = tokio::time::Instant::now();
        let fence = member_apply_fence(started, true).expect("a local member is fenced");
        assert!(
            fence - started
                >= Duration::from_secs(MEMBER_APPLY_DEADLINE_SECS + MEMBER_KILL_GRACE_SECS),
            "the fence outlasts the script's deadline and kill grace"
        );
        assert!(
            fence - started < MEMBER_APPLY_TIMEOUT,
            "and stays within the pass's own bound for one member"
        );
        assert_eq!(
            member_apply_fence(started, false),
            None,
            "a worker's agent holds the container until its exec stopped"
        );
    }

    #[tokio::test]
    async fn tls_passes_for_one_service_run_one_at_a_time() {
        let locks = ServiceTlsLocks::default();
        let first = locks.for_service(7).await;
        let held = first.lock().await;
        // A second pass for the same cluster waits for the first.
        assert!(locks.for_service(7).await.try_lock().is_err());
        // Another cluster is not held up.
        assert!(locks.for_service(8).await.try_lock().is_ok());
        drop(held);
        assert!(locks.for_service(7).await.try_lock().is_ok());
    }

    fn manager_without_data() -> ExternalServiceManager {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        ExternalServiceManager::new_with_handle(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "server-tls-lock-test",
            )),
            Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "server TLS lock test",
            )),
            true,
            Arc::new(temps_dns::DnsRegistry::new(db)),
        )
    }

    #[tokio::test]
    async fn a_mode_change_waits_while_another_change_or_pass_holds_the_service() {
        let manager = manager_without_data();
        let lock = manager.service_tls_locks.for_service(7).await;
        let held = lock.lock().await;
        let change = manager.set_service_tls_mode(7, ServerTlsMode::Off);
        tokio::pin!(change);
        // The mock database has no rows: a change that did not wait would
        // fail at once looking the service up, instead of still pending.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut change)
                .await
                .is_err(),
            "the change must not read or write anything while the lock is held"
        );
        drop(held);
        assert!(matches!(
            change.await,
            Err(ServerTlsError::Service(_)) | Err(ServerTlsError::NotFound { .. })
        ));
    }

    /// A standalone's initialization and start take the same lock as a TLS
    /// change: neither may rewrite the certificate directory or recreate
    /// the container while a change or its rollback does.
    #[tokio::test]
    async fn initialization_and_start_wait_while_a_tls_change_holds_the_service() {
        let manager = manager_without_data();
        let lock = manager.service_tls_locks.for_service(7).await;
        let held = lock.lock().await;
        let initialize = manager.initialize_service(7);
        let start = manager.start_service(7);
        tokio::pin!(initialize);
        tokio::pin!(start);
        // The mock database has no rows: one that did not wait would fail at
        // once looking the service up, instead of still pending.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut initialize)
                .await
                .is_err(),
            "initialization must wait for the TLS change"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut start)
                .await
                .is_err(),
            "a start must wait for the TLS change"
        );
        drop(held);
        assert!(initialize.await.is_err(), "it ran once the lock was free");
        assert!(start.await.is_err(), "it ran once the lock was free");
    }

    /// A request dropped while its change waits for (or holds) the TLS
    /// lock does not stop the change: it still runs to the end on its own
    /// task, so the lock is never released mid-change.
    #[tokio::test]
    async fn a_dropped_request_does_not_stop_its_change() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let manager = ExternalServiceManager::new_with_handle(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "server-tls-detach-test",
            )),
            Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "server TLS detach test",
            )),
            true,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        );
        let lock = manager.service_tls_locks.for_service(7).await;
        let held = lock.lock().await;
        {
            let mut change = Box::pin(manager.set_service_tls_mode(7, ServerTlsMode::Off));
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut change)
                    .await
                    .is_err(),
                "the change waits for the lock"
            );
            // The request goes away while its change waits.
        }
        drop(held);
        drop(lock);
        drop(manager);
        // The change's task holds the last other handles on the database
        // until it has run; then the log shows it looked the service up.
        let mut db = db;
        let mut owned = None;
        for _ in 0..100 {
            match Arc::try_unwrap(db) {
                Ok(connection) => {
                    owned = Some(connection);
                    break;
                }
                Err(shared) => {
                    db = shared;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
        let log = owned
            .expect("the change's task finished")
            .into_transaction_log();
        assert!(
            !log.is_empty(),
            "the dropped request's change never ran: it was cancelled with the request"
        );
    }

    /// A pass whose caller is dropped still schedules its bounded retry when
    /// it does not converge (here: the cluster cannot be read).
    #[tokio::test(start_paused = true)]
    async fn a_dropped_caller_does_not_lose_the_follow_up() {
        let mut mock = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres);
        for _ in 0..=FOLLOW_UP_ATTEMPTS {
            mock = mock.append_query_errors([sea_orm::DbErr::Custom("connection reset".into())]);
        }
        let db = Arc::new(mock.into_connection());
        let manager = ExternalServiceManager::new_with_handle(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "server-tls-follow-up-test",
            )),
            Arc::new(temps_core::DockerHandle::disabled(
                "control-plane",
                "server TLS follow-up test",
            )),
            true,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        );
        {
            let mut pass = Box::pin(manager.reconcile_cluster_server_tls(7, "test"));
            assert!(
                futures::poll!(pass.as_mut()).is_pending(),
                "the caller waits for its pass"
            );
            // The caller goes away before the pass has run.
        }
        drop(manager);
        // Paused time: the follow-up's sleeps pass at once. Its task holds
        // the last other handles on the database until it gives up.
        let mut db = db;
        let mut owned = None;
        for _ in 0..1000 {
            match Arc::try_unwrap(db) {
                Ok(connection) => {
                    owned = Some(connection);
                    break;
                }
                Err(shared) => {
                    db = shared;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
        let log = owned
            .expect("the pass and its follow-up finished")
            .into_transaction_log();
        assert!(
            log.len() > 1,
            "only the first pass ran ({} queries): its follow-up was lost with the caller",
            log.len()
        );
    }

    #[test]
    fn local_standalone_and_clustered_postgres_support_tls() {
        let none = HashMap::new();
        assert_eq!(
            server_tls_unsupported_reason(&service("postgres"), &none),
            None
        );

        assert!(server_tls_unsupported_reason(&service("redis"), &none)
            .unwrap()
            .contains("redis"));

        // A cluster is supported wherever its members run.
        let mut cluster = service("postgres");
        cluster.topology = "cluster".into();
        assert_eq!(server_tls_unsupported_reason(&cluster, &none), None);
        cluster.node_id = Some(3);
        assert_eq!(server_tls_unsupported_reason(&cluster, &none), None);
        let mut redis_cluster = service("redis");
        redis_cluster.topology = "cluster".into();
        assert!(server_tls_unsupported_reason(&redis_cluster, &none).is_some());

        let mut remote = service("postgres");
        remote.node_id = Some(3);
        assert!(server_tls_unsupported_reason(&remote, &none)
            .unwrap()
            .contains("worker nodes"));

        let imported = HashMap::from([(
            "container_name".to_string(),
            serde_json::json!("operator-db"),
        )]);
        assert!(
            server_tls_unsupported_reason(&service("postgres"), &imported)
                .unwrap()
                .contains("imported")
        );
    }

    #[test]
    fn new_local_postgres_and_clusters_default_to_available_and_nothing_else_does() {
        let none = HashMap::new();
        assert_eq!(
            default_tls_mode_for_new_service(ServiceType::Postgres, "standalone", None, &none),
            Some(ServerTlsMode::Available)
        );
        assert_eq!(
            default_tls_mode_for_new_service(ServiceType::Postgres, "cluster", None, &none),
            Some(ServerTlsMode::Available)
        );
        assert_eq!(
            default_tls_mode_for_new_service(ServiceType::Redis, "cluster", None, &none),
            None
        );
        assert_eq!(
            default_tls_mode_for_new_service(ServiceType::Postgres, "standalone", Some(2), &none),
            None
        );
        assert_eq!(
            default_tls_mode_for_new_service(ServiceType::Redis, "standalone", None, &none),
            None
        );
        let imported = HashMap::from([("container_name".to_string(), serde_json::json!("x"))]);
        assert_eq!(
            default_tls_mode_for_new_service(ServiceType::Postgres, "standalone", None, &imported),
            None
        );
    }

    #[test]
    fn postgres_certificates_name_the_container_overlay_and_loopback() {
        assert_eq!(
            postgres_subject_alt_names(&service("postgres")),
            vec![
                "postgres-orders".to_string(),
                "orders.temps.local".to_string(),
                "localhost".to_string(),
                "127.0.0.1".to_string(),
            ]
        );
    }

    #[test]
    fn container_state_reads_ssl_and_the_fingerprint_label() {
        let labels = HashMap::from([(SERVER_TLS_FINGERPRINT_LABEL.to_string(), "fp".to_string())]);
        assert_eq!(
            container_tls_state(
                &["postgres".into(), "-c".into(), "ssl=on".into()],
                Some(&labels)
            ),
            ContainerTlsState::On {
                fingerprint_sha256: Some("fp".into())
            }
        );
        assert_eq!(
            container_tls_state(&["postgres".into()], Some(&labels)),
            ContainerTlsState::Off
        );
    }

    fn status(
        mode: ServerTlsMode,
        container: ContainerTlsState,
        issued: Option<&str>,
    ) -> ServiceTlsStatus {
        ServiceTlsStatus {
            service_id: 7,
            mode,
            unsupported_reason: None,
            certificate: issued.map(|fp| IssuedCertificateInfo {
                fingerprint_sha256: fp.into(),
                subject_alt_names: vec![],
                not_before: Utc::now(),
                not_after: Utc::now(),
                authority_id: 1,
            }),
            authority: None,
            container,
            cluster: false,
            members: vec![],
        }
    }

    fn member(
        name: &str,
        role: &str,
        hostname: Option<&str>,
        ip: Option<&str>,
    ) -> ServiceMemberInfo {
        ServiceMemberInfo {
            id: 1,
            role: role.into(),
            node_id: None,
            container_name: name.into(),
            hostname: hostname.map(Into::into),
            port: Some(5432),
            status: "running".into(),
            ordinal: 1,
            compute_ip: ip.map(Into::into),
            provisioning_step: None,
            provisioning_error: None,
            live_state: None,
        }
    }

    #[test]
    fn cluster_certificates_name_the_vip_and_every_data_node_but_not_the_monitor() {
        let mut cluster = service("postgres");
        cluster.topology = "cluster".into();
        let members = [
            member("postgres-orders-monitor", "monitor", Some("10.0.0.9"), None),
            member(
                "postgres-orders-1",
                "replica",
                Some("postgres-orders-1.temps.local"),
                Some("10.210.0.4"),
            ),
            member("postgres-orders-2", "replica", Some("192.168.5.20"), None),
        ];
        assert_eq!(
            cluster_subject_alt_names(&cluster, &members),
            vec![
                "orders.temps.local".to_string(),
                "postgres-orders-1".to_string(),
                "postgres-orders-1.temps.local".to_string(),
                "10.210.0.4".to_string(),
                "postgres-orders-2".to_string(),
                "192.168.5.20".to_string(),
                "localhost".to_string(),
                "127.0.0.1".to_string(),
            ]
        );
    }

    #[test]
    fn cluster_members_are_classified_by_the_certificate_they_serve() {
        assert_eq!(
            ContainerTlsState::from_served_fingerprint("aa", Some("aa")),
            ContainerTlsState::On {
                fingerprint_sha256: Some("aa".into())
            }
        );
        assert_eq!(
            ContainerTlsState::from_served_fingerprint("bb", Some("aa")),
            ContainerTlsState::Unmanaged {
                fingerprint_sha256: "bb".into()
            }
        );
        assert_eq!(
            ContainerTlsState::from_served_fingerprint("", None),
            ContainerTlsState::Off
        );
        // Self-signed satisfies `off`, and lags `available`.
        let self_signed = ContainerTlsState::Unmanaged {
            fingerprint_sha256: "bb".into(),
        };
        assert!(!self_signed.lags(ServerTlsMode::Off, None));
        assert!(self_signed.lags(ServerTlsMode::Available, Some("aa")));
        assert_eq!(self_signed.serves_managed_certificate(), Some(false));
    }

    #[test]
    fn cluster_is_pending_while_any_data_node_lags() {
        let node = |name: &str, tls: ContainerTlsState| MemberTlsState {
            container_name: name.into(),
            status: "running".into(),
            tls,
            detail: None,
        };
        let managed = ContainerTlsState::On {
            fingerprint_sha256: Some("aa".into()),
        };
        let self_signed = ContainerTlsState::Unmanaged {
            fingerprint_sha256: "bb".into(),
        };
        let mut s = status(
            ServerTlsMode::Available,
            ContainerTlsState::Unknown,
            Some("aa"),
        );
        s.cluster = true;
        s.members = vec![node("n1", managed.clone()), node("n2", managed.clone())];
        assert!(!s.pending_restart());
        assert_eq!(agreed_member_state(&s.members), managed);

        s.members[1].tls = self_signed.clone();
        assert!(s.pending_restart());
        assert_eq!(
            s.lagging_members()
                .iter()
                .map(|m| m.container_name.as_str())
                .collect::<Vec<_>>(),
            vec!["n2"]
        );
        assert_eq!(agreed_member_state(&s.members), ContainerTlsState::Unknown);

        // An unreachable member is not counted as lagging.
        s.members[1].tls = ContainerTlsState::Unknown;
        assert!(!s.pending_restart());
        assert_eq!(agreed_member_state(&[]), ContainerTlsState::Unknown);
    }

    #[test]
    fn apply_report_lists_every_failed_member() {
        let mut report = ClusterTlsApplyReport {
            applied: vec!["n1".into()],
            skipped: vec![("n3".into(), "the member is stopped".into())],
            initializing: vec![],
            failed: vec![
                ("n2".into(), "exit 5: not served".into()),
                ("n4".into(), "no result within 120s".into()),
            ],
        };
        assert_eq!(
            report.describe_failures(),
            "n2: exit 5: not served; n4: no result within 120s"
        );
        assert!(!report.converged());
        report.failed.clear();
        // A stopped member waits for the next start; nothing to retry now.
        assert!(report.converged());
        report.initializing.push("n5".into());
        assert!(!report.converged());
    }

    #[test]
    fn pending_restart_compares_the_container_with_the_setting() {
        let on = |fp: &str| ContainerTlsState::On {
            fingerprint_sha256: Some(fp.into()),
        };
        assert!(!status(ServerTlsMode::Available, on("a"), Some("a")).pending_restart());
        assert!(status(ServerTlsMode::Available, on("a"), Some("b")).pending_restart());
        assert!(status(ServerTlsMode::Available, ContainerTlsState::Off, None).pending_restart());
        assert!(status(ServerTlsMode::Off, on("a"), Some("a")).pending_restart());
        assert!(!status(ServerTlsMode::Off, ContainerTlsState::Off, None).pending_restart());
        assert!(
            !status(ServerTlsMode::Available, ContainerTlsState::Unknown, None).pending_restart()
        );
    }
}
