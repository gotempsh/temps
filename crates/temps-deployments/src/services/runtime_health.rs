// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable runtime-health coordination. No probe execution occurs in a DB
//! transaction or via interactive/deployment exec.

use super::DeploymentService;
use chrono::{DateTime, Utc};
#[cfg(test)]
use futures::{stream, StreamExt};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use temps_core::{repo_config::HealthCheckConfig, DockerHandle};
use temps_runtime_health::{
    host::{HostIdentity, LeaseRequest, ProbeHost, ReportBatch, ReportRequest, TimedObservation},
    Assignment, ExecutionOwner, Probe,
};
use thiserror::Error;
use tokio::sync::OnceCell;
use utoipa::ToSchema;
use uuid::Uuid;

const BATCH: u64 = 100;
const LEASE_SECONDS: u64 = 120;

#[derive(Debug, Error)]
pub enum RuntimeHealthError {
    #[error("Runtime health database operation {operation} failed: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
    #[error("Runtime health configuration for deployment {deployment_id} is invalid")]
    Configuration { deployment_id: i32 },
    #[error("Runtime health report has stale or mismatched session, assignment or sequence")]
    StaleReport,
    #[error("Runtime health alarm delivery failed: {source}")]
    Alarm {
        #[source]
        source: temps_monitoring::AlarmError,
    },
    #[error("Runtime health management transport is unavailable for node {node_id:?}")]
    Transport { node_id: Option<i32> },
    #[error("Runtime health environment {environment_id} was not found in project {project_id}")]
    NotFound {
        project_id: i32,
        environment_id: i32,
    },
}

fn db_error(operation: &'static str, source: sea_orm::DbErr) -> RuntimeHealthError {
    RuntimeHealthError::Database { operation, source }
}
fn sql(query: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, query, values)
}

/// Safe, immutable configuration. It intentionally cannot contain exec argv.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveProbeRevision {
    pub kind: String,
    pub source: String,
    pub probe: Option<Probe>,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub failure_threshold: u32,
    pub recovery_threshold: u32,
    pub desired_replicas: u32,
    pub unavailable_reason: Option<String>,
}

impl EffectiveProbeRevision {
    pub fn resolve(
        health: &HealthCheckConfig,
        port: Option<u16>,
        path: Option<&str>,
        source: &str,
        replicas: u32,
    ) -> Self {
        let kind = health.kind(port.is_some()).to_string();
        let probe = match kind.as_str() {
            "process" => Some(Probe::Process {
                stable_for_secs: health.stable_for,
            }),
            "http" => Some(Probe::Http {
                path: path.unwrap_or(&health.path).to_string(),
                expected_status: health.status,
            }),
            "tcp" => Some(Probe::Tcp {}),
            _ => None,
        };
        let unavailable_reason = if kind == "exec" {
            Some("runtime_exec_unavailable".into())
        } else if probe.is_none()
            || !(1..=30).contains(&health.timeout)
            || !(1..=3600).contains(&health.interval)
            || !(1..=1000).contains(&replicas)
        {
            Some("runtime_configuration_invalid".into())
        } else {
            None
        };
        Self {
            kind,
            source: source.into(),
            probe,
            interval_seconds: health.interval,
            timeout_seconds: health.timeout,
            failure_threshold: health.retries.clamp(1, 1000),
            recovery_threshold: 2,
            desired_replicas: replicas,
            unavailable_reason,
        }
    }
}

/// Called by completion only after all container rows have been recorded. A
/// retry never mutates an existing authorized revision.
pub async fn save_revision(
    db: &DatabaseConnection,
    project_id: i32,
    environment_id: i32,
    deployment_id: i32,
    configuration: EffectiveProbeRevision,
) -> Result<(), RuntimeHealthError> {
    let encoded = serde_json::to_value(&configuration)
        .map_err(|_| RuntimeHealthError::Configuration { deployment_id })?;
    let transaction = db
        .begin()
        .await
        .map_err(|e| db_error("begin revision persistence", e))?;
    transaction.execute(sql("INSERT INTO runtime_health_revisions(id,project_id,environment_id,deployment_id,configuration) SELECT $1,$2,$3,$4,$5 FROM deployments d WHERE d.id=$4 AND d.project_id=$2 AND d.environment_id=$3 ON CONFLICT(deployment_id) DO NOTHING", vec![Uuid::new_v4().into(),project_id.into(),environment_id.into(),deployment_id.into(),encoded.into()])).await.map_err(|e| db_error("persist effective revision", e))?;
    // Assignment IDs are generated by the application; no pgcrypto extension
    // or database administrator privilege is required by the migration.
    let containers = transaction.query_all(sql("SELECT c.id,c.node_id,r.id AS revision_id FROM deployment_containers c JOIN runtime_health_revisions r ON r.deployment_id=c.deployment_id WHERE c.deployment_id=$1 AND c.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM runtime_health_assignments a WHERE a.container_record_id=c.id) ORDER BY c.id LIMIT 1000", vec![deployment_id.into()])).await.map_err(|e| db_error("load revision containers", e))?;
    let mut assignments = Vec::with_capacity(containers.len());
    for row in containers {
        let id: i32 = row
            .try_get("", "id")
            .map_err(|e| db_error("decode revision container", e))?;
        let node: Option<i32> = row
            .try_get("", "node_id")
            .map_err(|e| db_error("decode revision node", e))?;
        let revision: Uuid = row
            .try_get("", "revision_id")
            .map_err(|e| db_error("decode revision identity", e))?;
        assignments.push(serde_json::json!({"id":Uuid::new_v4(),"revision_id":revision,"container_record_id":id,"node_id":node}));
    }
    if !assignments.is_empty() {
        transaction.execute(sql("INSERT INTO runtime_health_assignments(id,revision_id,container_record_id,node_id) SELECT id,revision_id,container_record_id,node_id FROM jsonb_to_recordset($1) AS replica(id uuid,revision_id uuid,container_record_id integer,node_id integer) ON CONFLICT(container_record_id) DO NOTHING",vec![serde_json::Value::Array(assignments).into()])).await.map_err(|e|db_error("persist replica assignment batch",e))?;
    }
    transaction
        .commit()
        .await
        .map_err(|e| db_error("commit revision persistence", e))?;
    Ok(())
}

#[derive(Clone, Debug, FromQueryResult)]
struct Placement {
    id: Uuid,
    revision_id: Uuid,
    container_record_id: i32,
    node_id: Option<i32>,
    project_id: i32,
    environment_id: i32,
    deployment_id: i32,
    container_id: String,
    loaded_at: DateTime<Utc>,
    container_port: Option<i32>,
    configuration: serde_json::Value,
    session: Option<Uuid>,
    lease_until: Option<DateTime<Utc>>,
    current: bool,
    monitoring: bool,
    epoch: i64,
    terminal: bool,
    accounting_due: bool,
}

#[derive(Clone)]
enum Transport {
    Embedded(Arc<ProbeHost>),
    Remote(Arc<temps_deployer::remote::RemoteNodeDeployer>),
}
impl Transport {
    async fn identity(&self) -> Result<HostIdentity, RuntimeHealthError> {
        match self {
            Self::Embedded(host) => Ok(host.identity()),
            Self::Remote(remote) => remote
                .runtime_health_request("", None::<&()>)
                .await
                .map_err(|_| RuntimeHealthError::Transport { node_id: None }),
        }
    }
    async fn authorize(&self, request: LeaseRequest) -> Result<(), RuntimeHealthError> {
        match self {
            Self::Embedded(host) => {
                let host = host.clone();
                tokio::task::spawn_blocking(move || host.authorize(request))
                    .await
                    .map_err(|_| RuntimeHealthError::Transport { node_id: None })?
                    .map_err(|_| RuntimeHealthError::Transport { node_id: None })
            }
            Self::Remote(remote) => remote
                .runtime_health_authorize(&request)
                .await
                .map_err(|_| RuntimeHealthError::Transport { node_id: None }),
        }
    }
    async fn revoke(&self, session: Uuid, id: Uuid, epoch: u64) -> Result<(), RuntimeHealthError> {
        match self {
            Self::Embedded(host) => {
                let host = host.clone();
                tokio::task::spawn_blocking(move || host.revoke(session, id, epoch))
                    .await
                    .map_err(|_| RuntimeHealthError::Transport { node_id: None })?
                    .map_err(|_| RuntimeHealthError::Transport { node_id: None })
            }
            Self::Remote(remote) => remote
                .runtime_health_revoke(&temps_runtime_health::host::RevokeRequest {
                    session,
                    assignment_id: id,
                    epoch,
                })
                .await
                .map_err(|_| RuntimeHealthError::Transport { node_id: None }),
        }
    }
    async fn report(&self, request: ReportRequest) -> Result<ReportBatch, RuntimeHealthError> {
        match self {
            Self::Embedded(host) => host
                .report(request)
                .map_err(|_| RuntimeHealthError::Transport { node_id: None }),
            Self::Remote(remote) => remote
                .runtime_health_request("report", Some(&request))
                .await
                .map_err(|_| RuntimeHealthError::Transport { node_id: None }),
        }
    }
}

#[async_trait::async_trait]
pub trait RuntimeHealthTransportResolver: Send + Sync {
    async fn worker(
        &self,
        node_id: i32,
    ) -> Result<Arc<temps_deployer::remote::RemoteNodeDeployer>, RuntimeHealthError>;
}

#[async_trait::async_trait]
impl RuntimeHealthTransportResolver for DeploymentService {
    async fn worker(
        &self,
        node_id: i32,
    ) -> Result<Arc<temps_deployer::remote::RemoteNodeDeployer>, RuntimeHealthError> {
        self.container_operations_for_node(Some(node_id))
            .await
            .map_err(|_| RuntimeHealthError::Transport {
                node_id: Some(node_id),
            })?
            .runtime_health_remote()
            .ok_or(RuntimeHealthError::Transport {
                node_id: Some(node_id),
            })
    }
}

pub struct RuntimeHealthService {
    db: Arc<DatabaseConnection>,
    workers: Arc<dyn RuntimeHealthTransportResolver>,
    docker: Arc<DockerHandle>,
    embedded: OnceCell<Arc<ProbeHost>>,
    coordinator: Uuid,
    current_term: std::sync::atomic::AtomicI64,
    started: std::sync::atomic::AtomicBool,
    maintenance: tokio::sync::Mutex<tokio::time::Instant>,
    alarms: std::sync::OnceLock<Arc<temps_monitoring::AlarmService>>,
}

impl RuntimeHealthService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        workers: Arc<dyn RuntimeHealthTransportResolver>,
        docker: Arc<DockerHandle>,
    ) -> Self {
        Self {
            db,
            workers,
            docker,
            embedded: OnceCell::new(),
            coordinator: Uuid::new_v4(),
            current_term: std::sync::atomic::AtomicI64::new(0),
            started: std::sync::atomic::AtomicBool::new(false),
            maintenance: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            alarms: std::sync::OnceLock::new(),
        }
    }
    async fn transport(&self, node_id: Option<i32>) -> Result<Transport, RuntimeHealthError> {
        if let Some(node_id) = node_id {
            return self.workers.worker(node_id).await.map(Transport::Remote);
        }
        let host = self
            .embedded
            .get_or_try_init(|| async {
                let docker = self
                    .docker
                    .cloned()
                    .ok_or(RuntimeHealthError::Transport { node_id: None })?;
                let host = temps_deployer::runtime_health::open_host(
                    docker,
                    temps_deployer::runtime_health::data_root(),
                    false,
                )
                .await
                .map_err(|_| RuntimeHealthError::Transport { node_id: None })?;
                temps_deployer::runtime_health::spawn_host(host.clone());
                Ok(host)
            })
            .await?;
        Ok(Transport::Embedded(host.clone()))
    }
    pub fn set_alarm_service(&self, alarms: Arc<temps_monitoring::AlarmService>) {
        if self.alarms.set(alarms).is_err() {
            tracing::warn!("Runtime health alarm service was already registered");
        }
    }
    pub fn start(self: Arc<Self>) {
        use std::sync::atomic::Ordering;
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let maintenance = self.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let term = maintenance.current_term.load(Ordering::Acquire);
                if term == 0 {
                    continue;
                }
                if let Err(error) = maintenance.dispatch_alarms(term).await {
                    tracing::warn!(%error,"Runtime alarm delivery failed");
                }
                if let Err(error) = maintenance.expire_environments(term).await {
                    tracing::warn!(%error,"Runtime health expiry accounting failed");
                }
                if let Err(error) = maintenance.prune_history(term).await {
                    tracing::warn!(%error,"Runtime history maintenance failed");
                }
            }
        });
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(500));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut tasks = tokio::task::JoinSet::new();
            let mut pending =
                std::collections::HashMap::<tokio::task::Id, (Option<i32>, usize)>::new();
            loop {
                tokio::select! {
                    _=timer.tick()=>{
                        let occupied=pending.values().map(|(_,count)| *count).sum::<usize>();
                        let limit=if pending.len()<16 { BATCH.saturating_sub(occupied as u64) } else { 0 };
                        let blocked=pending.values().map(|(node,_)| node.unwrap_or(0)).collect::<Vec<_>>();
                        match self.take_due(blocked,limit).await {
                            Ok(Some((term,rows)))=>{
                                self.current_term.store(term,Ordering::Release);
                                let mut groups=BTreeMap::<Option<i32>,Vec<Placement>>::new();
                                for row in rows { groups.entry(row.node_id).or_default().push(row); }
                                for (node,rows) in groups {
                                    if pending.len()>=16 { break; }
                                    let count=rows.len();let service=self.clone();
                                    let task=tasks.spawn(async move { service.process_node(node,rows,term).await });
                                    pending.insert(task.id(),(node,count));
                                }
                            }
                            Ok(None)=>{ self.current_term.store(0,Ordering::Release); }
                            Err(error)=>{ self.current_term.store(0,Ordering::Release);tracing::warn!(%error,"Runtime health reconciliation failed"); }
                        }
                    }
                    Some(completed)=tasks.join_next_with_id(),if !tasks.is_empty()=>{
                        match completed {
                            Ok((id,result))=>{ pending.remove(&id);if let Err(error)=result { tracing::warn!(%error,"Runtime health node reconciliation failed"); } }
                            Err(error)=>{ pending.remove(&error.id());tracing::error!(%error,"Runtime health node task failed"); }
                        }
                    }
                }
            }
        });
    }

    async fn process_node(
        &self,
        node: Option<i32>,
        rows: Vec<Placement>,
        term: i64,
    ) -> Result<(), RuntimeHealthError> {
        let mut environments = rows
            .iter()
            .filter(|row| row.accounting_due)
            .map(|row| row.environment_id)
            .collect::<std::collections::BTreeSet<_>>();
        let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
        match tokio::time::timeout(
            Duration::from_secs(10),
            self.reconcile_node(node, rows, term, &mut environments),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(node_id=?node,%error,"Runtime health node is unavailable")
            }
            Err(_) => {
                tracing::warn!(node_id=?node,"Runtime health node reconciliation exceeded its budget")
            }
        }
        for environment in environments {
            self.account_environment(environment, term).await?;
        }
        self.stage_alarms(ids, term).await
    }

    #[cfg(test)]
    async fn reconcile(&self) -> Result<(), RuntimeHealthError> {
        let Some((term, rows)) = self.take_due(Vec::new(), BATCH).await? else {
            return Ok(());
        };
        let mut groups = BTreeMap::<Option<i32>, Vec<Placement>>::new();
        for row in rows {
            groups.entry(row.node_id).or_default().push(row);
        }
        let results = stream::iter(groups)
            .map(|(node, rows)| self.process_node(node, rows, term))
            .buffer_unordered(16)
            .collect::<Vec<_>>()
            .await;
        for result in results {
            result?;
        }
        self.dispatch_alarms(term).await?;
        self.prune_history(term).await
    }

    async fn take_due(
        &self,
        blocked: Vec<i32>,
        limit: u64,
    ) -> Result<Option<(i64, Vec<Placement>)>, RuntimeHealthError> {
        let lease = self.db.query_one(sql("INSERT INTO runtime_health_coordinator(singleton,owner,expires_at) VALUES(TRUE,$1,clock_timestamp()+interval '30 seconds') ON CONFLICT(singleton) DO UPDATE SET owner=EXCLUDED.owner, term=CASE WHEN runtime_health_coordinator.owner=EXCLUDED.owner THEN runtime_health_coordinator.term ELSE nextval('runtime_health_epoch') END, expires_at=EXCLUDED.expires_at WHERE runtime_health_coordinator.owner=EXCLUDED.owner OR runtime_health_coordinator.expires_at<=clock_timestamp() RETURNING term", vec![self.coordinator.into()])).await.map_err(|e| db_error("claim coordinator lease", e))?;
        let Some(lease) = lease else {
            return Ok(None);
        };
        let term: i64 = lease
            .try_get("", "term")
            .map_err(|e| db_error("decode coordinator term", e))?;
        let query = "SELECT a.*,clock_timestamp() AS loaded_at,r.project_id,r.environment_id,r.deployment_id,r.configuration,c.container_id,c.container_port, (a.retired_at IS NULL AND e.current_deployment_id=r.deployment_id AND d.state IN ('completed','deployed') AND c.deleted_at IS NULL AND NOT p.is_deleted AND c.node_id IS NOT DISTINCT FROM a.node_id) AS current, (COALESCE(h.enabled,TRUE) AND NOT COALESCE(h.paused,FALSE) AND NOT e.sleeping) AS monitoring, ((a.activated_at IS NOT NULL AND (e.current_deployment_id IS NULL OR EXISTS(SELECT 1 FROM deployments selected WHERE selected.id=e.current_deployment_id AND selected.state IN ('completed','deployed')))) OR d.state IN ('failed','cancelled','canceled','stopped') OR c.deleted_at IS NOT NULL OR p.is_deleted OR c.node_id IS DISTINCT FROM a.node_id) AS terminal, (h.environment_id IS NULL OR h.revision_id IS DISTINCT FROM r.id OR h.accounted_excluded IS DISTINCT FROM (NOT COALESCE(h.enabled,TRUE) OR COALESCE(h.paused,FALSE) OR e.sleeping) OR (h.state<>'unknown' AND h.valid_until<=clock_timestamp()) OR COALESCE(a.observed_at>h.accounted_at,FALSE)) AS accounting_due FROM runtime_health_assignments a JOIN runtime_health_revisions r ON r.id=a.revision_id JOIN deployment_containers c ON c.id=a.container_record_id JOIN environments e ON e.id=r.environment_id JOIN projects p ON p.id=r.project_id JOIN deployments d ON d.id=r.deployment_id LEFT JOIN runtime_health_environments h ON h.environment_id=e.id WHERE a.next_reconcile_at<=clock_timestamp() AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements_text($2) blocked WHERE blocked.value::int=COALESCE(a.node_id,0)) ORDER BY a.next_reconcile_at,a.id LIMIT $1";
        let blocked = serde_json::to_value(blocked)
            .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
        let rows = Placement::find_by_statement(sql(
            query,
            vec![(limit.min(BATCH) as i64).into(), blocked.into()],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(|e| db_error("load due assignments", e))?;
        Ok(Some((term, rows)))
    }

    async fn reconcile_node(
        &self,
        node: Option<i32>,
        rows: Vec<Placement>,
        term: i64,
        changed: &mut std::collections::BTreeSet<i32>,
    ) -> Result<(), RuntimeHealthError> {
        // Claim this batch in one write. Pending completion revisions remain
        // pending; they must never be retired or probed before activation.
        let ids = serde_json::to_value(rows.iter().map(|row| row.id).collect::<Vec<_>>())
            .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
        self.db.execute(sql("UPDATE runtime_health_assignments a SET next_reconcile_at=clock_timestamp()+interval '5 seconds',activated_at=CASE WHEN e.current_deployment_id=r.deployment_id AND d.state IN ('completed','deployed') THEN COALESCE(a.activated_at,clock_timestamp()) ELSE a.activated_at END,retired_at=CASE WHEN (e.current_deployment_id IS DISTINCT FROM r.deployment_id OR d.state NOT IN ('completed','deployed') OR c.deleted_at IS NOT NULL OR p.is_deleted OR c.node_id IS DISTINCT FROM a.node_id) AND ((a.activated_at IS NOT NULL AND (e.current_deployment_id IS NULL OR EXISTS(SELECT 1 FROM deployments selected WHERE selected.id=e.current_deployment_id AND selected.state IN ('completed','deployed')))) OR d.state IN ('failed','cancelled','canceled','stopped') OR c.deleted_at IS NOT NULL OR p.is_deleted OR c.node_id IS DISTINCT FROM a.node_id) THEN COALESCE(a.retired_at,clock_timestamp()) ELSE a.retired_at END FROM runtime_health_revisions r,deployments d,environments e,deployment_containers c,projects p WHERE a.id IN (SELECT value::uuid FROM jsonb_array_elements_text($1)) AND r.id=a.revision_id AND d.id=r.deployment_id AND e.id=r.environment_id AND c.id=a.container_record_id AND p.id=r.project_id AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$2 AND term=$3 AND expires_at>clock_timestamp())",vec![ids.into(),self.coordinator.into(),term.into()])).await.map_err(|e| db_error("claim assignment reconciliation batch", e))?;
        if rows.iter().all(|row| !row.current && !row.terminal) {
            return Ok(());
        }
        let expired = rows
            .iter()
            .filter(|row| {
                ((!row.current && row.terminal) || !row.monitoring)
                    && row.lease_until.is_none_or(|until| until <= Utc::now())
            })
            .map(|row| row.id)
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            let ids = serde_json::to_value(&expired)
                .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
            self.db.execute(sql("UPDATE runtime_health_assignments a SET next_reconcile_at='infinity'::timestamptz,valid_until=clock_timestamp(),state='unknown',reason='monitoring_inactive' WHERE a.id IN (SELECT value::uuid FROM jsonb_array_elements_text($1)) AND (a.lease_until IS NULL OR a.lease_until<=clock_timestamp()) AND (a.retired_at IS NOT NULL OR EXISTS(SELECT 1 FROM runtime_health_revisions r JOIN environments e ON e.id=r.environment_id LEFT JOIN runtime_health_environments h ON h.environment_id=e.id WHERE r.id=a.revision_id AND (e.sleeping OR NOT COALESCE(h.enabled,TRUE) OR COALESCE(h.paused,FALSE)))) AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$2 AND term=$3 AND expires_at>clock_timestamp())",vec![ids.into(),self.coordinator.into(),term.into()])).await.map_err(|e| db_error("park revoked runtime assignments",e))?;
        }
        let rows = rows
            .into_iter()
            .filter(|row| !expired.contains(&row.id))
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Ok(());
        }
        let transport = match self.transport(node).await {
            Ok(transport) => transport,
            Err(error) => {
                tracing::debug!(node_id=?node, %error, "Runtime health node is unavailable");
                return Ok(());
            }
        };
        let identity = transport.identity().await?;
        if identity.protocol_version != 1
            || !matches!(
                (node, identity.owner),
                (None, ExecutionOwner::Embedded(_)) | (Some(_), ExecutionOwner::Agent(_))
            )
        {
            return Err(RuntimeHealthError::Transport { node_id: node });
        }
        let mut authorized = Vec::new();
        for row in rows {
            if !row.current || !row.monitoring {
                if !row.current && !row.terminal {
                    continue;
                }
                let mut quiescent = row.session != Some(identity.session)
                    || row.lease_until.is_none_or(|until| until <= Utc::now());
                if !quiescent {
                    quiescent = transport
                        .revoke(identity.session, row.id, row.epoch as u64)
                        .await
                        .is_ok();
                }
                self.db.execute(sql("UPDATE runtime_health_assignments SET lease_until=CASE WHEN $3 THEN clock_timestamp() ELSE lease_until END,next_reconcile_at=CASE WHEN $3 THEN 'infinity'::timestamptz ELSE clock_timestamp()+interval '5 seconds' END,valid_until=clock_timestamp(),state='unknown',reason='monitoring_inactive' WHERE id=$1 AND epoch=$2 AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$4 AND term=$5 AND expires_at>clock_timestamp())",vec![row.id.into(),row.epoch.into(),quiescent.into(),self.coordinator.into(),term.into()])).await.map_err(|e| db_error("revoke inactive monitoring",e))?;
                continue;
            }
            let config: EffectiveProbeRevision = serde_json::from_value(row.configuration.clone())
                .map_err(|_| RuntimeHealthError::Configuration {
                    deployment_id: row.deployment_id,
                })?;
            if config.unavailable_reason.is_some() {
                continue;
            }
            let Some(probe) = config.probe.clone() else {
                continue;
            };
            let needs_renewal = row.session != Some(identity.session)
                || row
                    .lease_until
                    .is_none_or(|until| until < Utc::now() + chrono::Duration::seconds(60));
            if needs_renewal {
                let owner = serde_json::to_value(identity.owner).map_err(|_| {
                    RuntimeHealthError::Configuration {
                        deployment_id: row.deployment_id,
                    }
                })?;
                let renewed = self.db.query_one(sql("UPDATE runtime_health_assignments SET epoch=CASE WHEN session IS DISTINCT FROM $2 OR lease_until IS NULL OR lease_until<=clock_timestamp() THEN nextval('runtime_health_epoch') ELSE epoch END, last_sequence=CASE WHEN session IS DISTINCT FROM $2 OR lease_until IS NULL OR lease_until<=clock_timestamp() THEN 0 ELSE last_sequence END, session=$2,execution_owner=$3,renewal=renewal+1,lease_until=clock_timestamp()+interval '120 seconds' WHERE id=$1 AND retired_at IS NULL AND EXISTS(SELECT 1 FROM runtime_health_revisions r JOIN environments e ON e.id=r.environment_id JOIN deployments d ON d.id=r.deployment_id JOIN projects p ON p.id=r.project_id JOIN deployment_containers c ON c.id=runtime_health_assignments.container_record_id WHERE r.id=runtime_health_assignments.revision_id AND e.current_deployment_id=r.deployment_id AND d.state IN ('completed','deployed') AND NOT p.is_deleted AND NOT e.sleeping AND c.deleted_at IS NULL AND c.node_id IS NOT DISTINCT FROM runtime_health_assignments.node_id AND NOT EXISTS(SELECT 1 FROM runtime_health_environments h WHERE h.environment_id=e.id AND (h.paused OR NOT h.enabled))) AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$4 AND term=$5 AND expires_at>clock_timestamp()) RETURNING epoch,renewal",vec![row.id.into(),identity.session.into(),owner.into(),self.coordinator.into(),term.into()])).await.map_err(|e| db_error("persist execution lease", e))?;
                let Some(renewed) = renewed else {
                    continue;
                };
                let epoch: i64 = renewed
                    .try_get("", "epoch")
                    .map_err(|e| db_error("decode execution epoch", e))?;
                let renewal: i64 = renewed
                    .try_get("", "renewal")
                    .map_err(|e| db_error("decode renewal sequence", e))?;
                let assignment = Assignment {
                    id: row.id,
                    owner: identity.owner,
                    project_id: row.project_id as u32,
                    environment_id: row.environment_id as u32,
                    deployment_id: row.deployment_id as u32,
                    replica_id: row.container_record_id as u32,
                    placement_generation: row.container_record_id as u64,
                    revision: row.revision_id,
                    epoch: epoch as u64,
                    container_id: row.container_id.clone(),
                    container_port: row.container_port.and_then(|port| u16::try_from(port).ok()),
                    probe,
                    interval_secs: config.interval_seconds,
                    timeout_secs: config.timeout_seconds,
                };
                if transport
                    .authorize(LeaseRequest {
                        authorization_window: identity.authorization_window,
                        session: identity.session,
                        renewal: renewal as u64,
                        lease_secs: LEASE_SECONDS - 10,
                        assignment,
                    })
                    .await
                    .is_err()
                {
                    // Do not keep an unacknowledged lease looking authorized.
                    let rejection = self
                        .db
                        .begin()
                        .await
                        .map_err(|e| db_error("begin rejected authorization", e))?;
                    // Match policy updates' environment-before-assignment lock
                    // order while ending the previous coverage atomically.
                    rejection.query_one(sql("SELECT environment_id FROM runtime_health_environments WHERE environment_id=$1 FOR UPDATE",vec![row.environment_id.into()])).await.map_err(|e| db_error("lock rejected authorization coverage", e))?;
                    rejection.execute(sql("WITH rejected AS (UPDATE runtime_health_assignments SET lease_until=clock_timestamp(),valid_until=clock_timestamp(),unknown_since=CASE WHEN state='unknown' AND reason='authorization_failed' THEN COALESCE(unknown_since,clock_timestamp()) ELSE clock_timestamp() END,state='unknown',reason='authorization_failed' WHERE id=$1 AND epoch=$2 AND renewal=$3 AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$4 AND term=$5 AND expires_at>clock_timestamp()) RETURNING revision_id,valid_until) UPDATE runtime_health_environments h SET valid_until=LEAST(h.valid_until,rejected.valid_until) FROM rejected WHERE h.revision_id=rejected.revision_id",vec![row.id.into(),epoch.into(),renewal.into(),self.coordinator.into(),term.into()])).await.map_err(|e| db_error("expire rejected lease", e))?;
                    rejection
                        .commit()
                        .await
                        .map_err(|e| db_error("commit rejected authorization", e))?;
                    changed.insert(row.environment_id);
                    continue;
                }
            }
            authorized.push(row);
        }
        if authorized.is_empty() {
            return Ok(());
        }
        let collection = self.db.query_one(sql("INSERT INTO runtime_health_collections(node_key,coordinator) SELECT $1,$2 WHERE EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$2 AND term=$3 AND expires_at>clock_timestamp()) ON CONFLICT(node_key) DO UPDATE SET coordinator=EXCLUDED.coordinator,epoch=CASE WHEN runtime_health_collections.coordinator<>EXCLUDED.coordinator OR runtime_health_collections.last_success_at IS NULL OR runtime_health_collections.last_success_at<clock_timestamp()-interval '15 seconds' THEN nextval('runtime_health_epoch') ELSE runtime_health_collections.epoch END WHERE EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$2 AND term=$3 AND expires_at>clock_timestamp()) RETURNING epoch",vec![node.unwrap_or(0).into(),self.coordinator.into(),term.into()])).await.map_err(|e| db_error("fence reporting session", e))?;
        let Some(collection) = collection else {
            return Ok(());
        };
        let collection_epoch: i64 = collection
            .try_get("", "epoch")
            .map_err(|e| db_error("decode reporting epoch", e))?;
        let challenge = Uuid::new_v4();
        let started = tokio::time::Instant::now();
        let request = ReportRequest {
            collection_epoch: collection_epoch as u64,
            session: identity.session,
            challenge,
            assignments: authorized.iter().map(|row| row.id).collect(),
        };
        let batch = tokio::time::timeout(Duration::from_secs(5), transport.report(request))
            .await
            .map_err(|_| RuntimeHealthError::Transport { node_id: node })??;
        if started.elapsed() > Duration::from_secs(5)
            || batch.collection_epoch != collection_epoch as u64
            || batch.session != identity.session
            || batch.challenge != challenge
            || batch.observations.len() > authorized.len()
        {
            return Err(RuntimeHealthError::StaleReport);
        }
        self.db.execute(sql("UPDATE runtime_health_collections SET last_success_at=clock_timestamp() WHERE node_key=$1 AND epoch=$2 AND coordinator=$3",vec![node.unwrap_or(0).into(),collection_epoch.into(),self.coordinator.into()])).await.map_err(|e| db_error("acknowledge reporting session", e))?;
        let accepted = self
            .accept_batch(&authorized, identity.session, batch.observations, term)
            .await?;
        for row in authorized.iter().filter(|row| accepted.contains(&row.id)) {
            changed.insert(row.environment_id);
        }
        Ok(())
    }

    #[cfg(test)]
    async fn accept(
        &self,
        row: &Placement,
        session: Uuid,
        sample: TimedObservation,
        term: i64,
    ) -> Result<bool, RuntimeHealthError> {
        Ok(!self
            .accept_batch(std::slice::from_ref(row), session, vec![sample], term)
            .await?
            .is_empty())
    }

    async fn accept_batch(
        &self,
        placements: &[Placement],
        session: Uuid,
        samples: Vec<TimedObservation>,
        term: i64,
    ) -> Result<Vec<Uuid>, RuntimeHealthError> {
        if samples.len() > BATCH as usize || samples.len() > placements.len() {
            return Err(RuntimeHealthError::StaleReport);
        }
        let mut seen = std::collections::HashSet::new();
        let mut validated = Vec::with_capacity(samples.len());
        for sample in samples {
            let observation = sample.observation;
            let row = placements
                .iter()
                .find(|row| row.id == observation.assignment_id)
                .ok_or(RuntimeHealthError::StaleReport)?;
            let config: EffectiveProbeRevision = serde_json::from_value(row.configuration.clone())
                .map_err(|_| RuntimeHealthError::Configuration {
                    deployment_id: row.deployment_id,
                })?;
            let freshness = config.interval_seconds * 3 + config.timeout_seconds;
            if !seen.insert(observation.assignment_id)
                || observation.revision != row.revision_id
                || observation.placement_generation != row.container_record_id as u64
                || observation.sequence == 0
                || sample.age_ms >= freshness * 1000
                || observation.sequence > i64::MAX as u64
                || observation.epoch > i64::MAX as u64
            {
                return Err(RuntimeHealthError::StaleReport);
            }
            // Receipt starts coverage; age only shortens forward freshness.
            // All rows are validated before one atomic current/history write.
            validated.push(serde_json::json!({
                "id":row.id,"revision":row.revision_id,"container_id":row.container_id,
                "epoch":observation.epoch as i64,"sequence":observation.sequence as i64,
                "state":observation.state,"reason":observation.reason,
                "deadline": row.loaded_at + chrono::Duration::milliseconds((freshness * 1000 - sample.age_ms) as i64),
            }));
        }
        if validated.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.db.query_all(sql(r#"
WITH accepted AS (
 UPDATE runtime_health_assignments a SET
  state=sample.state,reason=sample.reason,last_sequence=sample.sequence,
  observed_at=statement_timestamp(),
  valid_until=sample.deadline,
  failure_streak=CASE WHEN sample.state='unhealthy' THEN
   CASE WHEN a.valid_until>clock_timestamp() THEN LEAST(a.failure_streak+1,1000) ELSE 1 END ELSE 0 END,
  recovery_streak=CASE WHEN sample.state='healthy' THEN
   CASE WHEN a.valid_until>clock_timestamp() THEN LEAST(a.recovery_streak+1,1000) ELSE 1 END ELSE 0 END,
  unknown_since=CASE WHEN sample.state='unknown' THEN COALESCE(a.unknown_since,clock_timestamp()) ELSE NULL END
 FROM jsonb_to_recordset($1) AS sample(id uuid,revision uuid,container_id text,epoch bigint,sequence bigint,state text,reason text,deadline timestamptz),
  runtime_health_revisions r,deployment_containers c,environments e,deployments d,projects p
 WHERE sample.deadline>clock_timestamp() AND a.id=sample.id AND a.session=$2 AND a.epoch=sample.epoch AND a.last_sequence<sample.sequence
  AND a.lease_until>clock_timestamp() AND a.retired_at IS NULL
  AND r.id=a.revision_id AND r.id=sample.revision AND p.id=r.project_id AND NOT p.is_deleted
  AND c.id=a.container_record_id AND c.container_id=sample.container_id
  AND c.node_id IS NOT DISTINCT FROM a.node_id AND c.deleted_at IS NULL
  AND e.id=r.environment_id AND e.current_deployment_id=r.deployment_id AND NOT e.sleeping
  AND d.id=r.deployment_id AND d.state IN ('completed','deployed')
  AND NOT EXISTS(SELECT 1 FROM runtime_health_environments h WHERE h.environment_id=e.id AND (NOT h.enabled OR h.paused))
  AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$3 AND term=$4 AND expires_at>clock_timestamp())
 RETURNING a.id,a.epoch,a.last_sequence,a.state,a.reason,a.valid_until
), stored AS (
 INSERT INTO runtime_health_samples(assignment_id,epoch,sequence,state,reason,valid_until)
 SELECT id,epoch,last_sequence,state,reason,valid_until FROM accepted ON CONFLICT DO NOTHING RETURNING assignment_id
)
SELECT assignment_id FROM stored
"#,vec![serde_json::Value::Array(validated).into(),session.into(),self.coordinator.into(),term.into()])).await.map_err(|e|db_error("accept fenced observation batch",e))?;
        rows.into_iter()
            .map(|row| {
                row.try_get("", "assignment_id")
                    .map_err(|e| db_error("decode accepted observation", e))
            })
            .collect()
    }
}

#[derive(FromQueryResult)]
struct Accounting {
    revision_id: Option<Uuid>,
    state: String,
    valid_until: DateTime<Utc>,
    accounted_at: DateTime<Utc>,
    enabled: bool,
    paused: bool,
    required_healthy: Option<i32>,
    now: DateTime<Utc>,
    accounted_excluded: bool,
    new_observation_at: Option<DateTime<Utc>>,
}

#[derive(FromQueryResult)]
struct ActiveRevision {
    id: Uuid,
    configuration: serde_json::Value,
    created_at: DateTime<Utc>,
    sleeping: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHealthPolicy {
    pub enabled: bool,
    pub paused: bool,
    pub required_healthy: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeProbeDisplay {
    pub kind: String,
    pub source: String,
    pub description: String,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub failure_threshold: u32,
    pub recovery_threshold: u32,
    pub desired_replicas: u32,
    pub required_healthy: u32,
    pub unavailable_reason: Option<String>,
}

impl EffectiveProbeRevision {
    fn display(&self, required: u32) -> RuntimeProbeDisplay {
        let description = match &self.probe {
            Some(Probe::Process { stable_for_secs }) => format!("Container Running with unchanged restart count for {stable_for_secs}s. This does not check application progress."),
            Some(Probe::Http { path, expected_status }) => format!("HTTP GET {} → {expected_status}", path.split('?').next().unwrap_or("/")),
            Some(Probe::Tcp {}) => "Connect to the container's declared TCP port".into(),
            None => "Exec is checked at deployment only. Runtime exec is unavailable until non-destructive cleanup is supported. Command arguments are hidden.".into(),
        };
        RuntimeProbeDisplay {
            kind: self.kind.clone(),
            source: self.source.clone(),
            description,
            interval_seconds: self.interval_seconds,
            timeout_seconds: self.timeout_seconds,
            failure_threshold: self.failure_threshold,
            recovery_threshold: self.recovery_threshold,
            desired_replicas: self.desired_replicas,
            required_healthy: required,
            unavailable_reason: self.unavailable_reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeCoverage {
    pub healthy_seconds: f64,
    pub unhealthy_seconds: f64,
    pub unknown_seconds: f64,
    pub excluded_seconds: f64,
    pub availability_percent: Option<f64>,
    pub coverage_percent: Option<f64>,
}

impl RuntimeCoverage {
    fn new(healthy: f64, unhealthy: f64, unknown: f64, excluded: f64) -> Self {
        let observed = healthy + unhealthy;
        let eligible = observed + unknown;
        Self {
            healthy_seconds: healthy,
            unhealthy_seconds: unhealthy,
            unknown_seconds: unknown,
            excluded_seconds: excluded,
            availability_percent: (observed > 0.0).then(|| (healthy / observed) * 100.0),
            coverage_percent: (eligible > 0.0).then(|| (observed / eligible) * 100.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeHealthBucket {
    #[schema(value_type = String, format = DateTime)]
    pub bucket_at: DateTime<Utc>,
    pub coverage: RuntimeCoverage,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeReplicaHealth {
    pub replica_id: i32,
    pub state: String,
    pub reason: String,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeHealthSummary {
    pub environment_id: i32,
    pub configured: bool,
    pub portless: Option<bool>,
    pub state: String,
    pub reason: String,
    pub probe: Option<RuntimeProbeDisplay>,
    pub policy: RuntimeHealthPolicy,
    pub coverage: RuntimeCoverage,
    pub replicas: Vec<RuntimeReplicaHealth>,
    pub history: Vec<RuntimeHealthBucket>,
}

fn aggregate_state(desired: u32, required: u32, healthy: u32, unhealthy: u32) -> &'static str {
    if required == 0 || required > desired {
        return "unknown";
    }
    if healthy >= required {
        "healthy"
    } else if desired.saturating_sub(unhealthy) < required {
        "unhealthy"
    } else {
        "unknown"
    }
}

async fn persist_coverage<C: ConnectionTrait>(
    connection: &C,
    environment_id: i32,
    accounting: &Accounting,
    previous_valid_until: DateTime<Utc>,
) -> Result<(), RuntimeHealthError> {
    if accounting.revision_id.is_none()
        || (!accounting.accounted_excluded && accounting.state == "unknown")
    {
        return Ok(());
    }
    let previous_valid_until = previous_valid_until.min(
        accounting
            .new_observation_at
            .unwrap_or(previous_valid_until),
    );
    connection
        .execute(sql(
            ACCOUNT_COVERAGE_SQL,
            vec![
                environment_id.into(),
                accounting.accounted_at.into(),
                accounting.now.into(),
                previous_valid_until.into(),
                accounting.state.clone().into(),
                accounting.accounted_excluded.into(),
                accounting.revision_id.into(),
            ],
        ))
        .await
        .map_err(|e| db_error("store uptime coverage buckets", e))?;
    Ok(())
}

const ACCOUNT_COVERAGE_SQL: &str = r#"
WITH bounds AS (
 SELECT GREATEST($2::timestamptz,$3::timestamptz-interval '30 days') AS start_at,CASE WHEN $6 THEN $3::timestamptz ELSE LEAST($4::timestamptz,$3::timestamptz) END AS end_at,LEAST($4::timestamptz,$3::timestamptz) AS valid_at
), parts AS (
 SELECT b AS bucket_at,
 GREATEST(0,EXTRACT(epoch FROM LEAST(b+interval '1 minute',end_at)-GREATEST(b,start_at)))::double precision AS total,
 GREATEST(0,EXTRACT(epoch FROM LEAST(b+interval '1 minute',valid_at,end_at)-GREATEST(b,start_at)))::double precision AS known
 FROM bounds, LATERAL generate_series(date_bin(interval '1 minute',start_at,'2000-01-01'::timestamptz),end_at,interval '1 minute') b
)
INSERT INTO runtime_health_buckets(environment_id,revision_id,bucket_at,healthy_seconds,unhealthy_seconds,unknown_seconds,excluded_seconds)
SELECT $1,$7,bucket_at,CASE WHEN NOT $6 AND $5='healthy' THEN known ELSE 0 END,
CASE WHEN NOT $6 AND $5='unhealthy' THEN known ELSE 0 END,
CASE WHEN $6 THEN 0 WHEN $5 IN ('healthy','unhealthy') THEN total-known ELSE total END,
CASE WHEN $6 THEN total ELSE 0 END FROM parts WHERE total>0
ON CONFLICT(environment_id,revision_id,bucket_at) DO UPDATE SET
 healthy_seconds=runtime_health_buckets.healthy_seconds+EXCLUDED.healthy_seconds,
 unhealthy_seconds=runtime_health_buckets.unhealthy_seconds+EXCLUDED.unhealthy_seconds,
 unknown_seconds=runtime_health_buckets.unknown_seconds+EXCLUDED.unknown_seconds,
 excluded_seconds=runtime_health_buckets.excluded_seconds+EXCLUDED.excluded_seconds
"#;

impl RuntimeHealthService {
    async fn account_environment(
        &self,
        environment_id: i32,
        term: i64,
    ) -> Result<(), RuntimeHealthError> {
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|e| db_error("begin uptime accounting", e))?;
        let leader = transaction.query_one(sql("SELECT term FROM runtime_health_coordinator WHERE owner=$1 AND term=$2 AND expires_at>clock_timestamp() FOR SHARE",vec![self.coordinator.into(),term.into()])).await.map_err(|e| db_error("fence uptime accounting", e))?;
        if leader.is_none() {
            return Ok(());
        }
        let revision = ActiveRevision::find_by_statement(sql("SELECT r.id,r.configuration,r.created_at,e.sleeping FROM runtime_health_revisions r JOIN environments e ON e.current_deployment_id=r.deployment_id JOIN deployments d ON d.id=r.deployment_id WHERE e.id=$1 AND d.state IN ('completed','deployed')",vec![environment_id.into()])).one(&transaction).await.map_err(|e| db_error("load current uptime revision", e))?;
        let Some(revision) = revision else {
            let accounting = Accounting::find_by_statement(sql("SELECT h.*,clock_timestamp() AS now,(SELECT MIN(a.observed_at) FROM runtime_health_assignments a WHERE a.revision_id=h.revision_id AND a.observed_at>h.accounted_at) AS new_observation_at FROM runtime_health_environments h WHERE h.environment_id=$1 FOR UPDATE OF h",vec![environment_id.into()])).one(&transaction).await.map_err(|e| db_error("lock retired uptime accounting", e))?;
            if let Some(accounting) = accounting {
                persist_coverage(
                    &transaction,
                    environment_id,
                    &accounting,
                    accounting.valid_until,
                )
                .await?;
                transaction.execute(sql("UPDATE runtime_health_environments SET state='unknown',reason='no_current_deployment',valid_until=$2,accounted_at=$2 WHERE environment_id=$1",vec![environment_id.into(),accounting.now.into()])).await.map_err(|e| db_error("retire unavailable uptime state", e))?;
            }
            transaction
                .commit()
                .await
                .map_err(|e| db_error("commit retired uptime accounting", e))?;
            return Ok(());
        };
        transaction.execute(sql("INSERT INTO runtime_health_environments(environment_id,revision_id) VALUES($1,$2) ON CONFLICT(environment_id) DO NOTHING",vec![environment_id.into(),revision.id.into()])).await.map_err(|e| db_error("initialize uptime accounting", e))?;
        let accounting = Accounting::find_by_statement(sql("SELECT h.*,clock_timestamp() AS now,(SELECT MIN(a.observed_at) FROM runtime_health_assignments a WHERE a.revision_id=h.revision_id AND a.observed_at>h.accounted_at) AS new_observation_at FROM runtime_health_environments h WHERE h.environment_id=$1 FOR UPDATE OF h",vec![environment_id.into()])).one(&transaction).await.map_err(|e| db_error("lock uptime accounting", e))?.ok_or(RuntimeHealthError::NotFound { project_id: 0, environment_id })?;
        let previous_valid_until = if accounting.revision_id == Some(revision.id) {
            accounting.valid_until
        } else {
            accounting.valid_until.min(revision.created_at)
        };
        // Bound catch-up and retention to 30 days. Each bucket covers one minute;
        // missing collection intervals become unknown, never retroactive green.
        persist_coverage(
            &transaction,
            environment_id,
            &accounting,
            previous_valid_until,
        )
        .await?;
        let config: EffectiveProbeRevision = serde_json::from_value(revision.configuration)
            .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
        let required = accounting
            .required_healthy
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(config.desired_replicas);
        let counts = transaction.query_one(sql("SELECT COUNT(*) FILTER(WHERE a.state='healthy')::bigint AS healthy,COUNT(*) FILTER(WHERE a.state='unhealthy')::bigint AS unhealthy,MIN(LEAST(a.valid_until,a.lease_until)) AS valid_until FROM runtime_health_assignments a JOIN deployment_containers c ON c.id=a.container_record_id WHERE a.revision_id=$1 AND a.retired_at IS NULL AND c.deleted_at IS NULL AND c.node_id IS NOT DISTINCT FROM a.node_id AND a.valid_until>$2 AND a.lease_until>$2",vec![revision.id.into(),accounting.now.into()])).await.map_err(|e| db_error("aggregate current replicas", e))?.ok_or(RuntimeHealthError::Configuration { deployment_id: 0 })?;
        let healthy = counts
            .try_get::<i64>("", "healthy")
            .map_err(|e| db_error("decode healthy replica count", e))? as u32;
        let unhealthy = counts
            .try_get::<i64>("", "unhealthy")
            .map_err(|e| db_error("decode unhealthy replica count", e))?
            as u32;
        let valid_until: Option<DateTime<Utc>> = counts
            .try_get("", "valid_until")
            .map_err(|e| db_error("decode aggregate freshness", e))?;
        let excluded = !accounting.enabled || accounting.paused || revision.sleeping;
        let state = if excluded || config.unavailable_reason.is_some() {
            "unknown"
        } else {
            aggregate_state(config.desired_replicas, required, healthy, unhealthy)
        };
        let reason = if revision.sleeping {
            "sleeping"
        } else if accounting.paused {
            "paused"
        } else if !accounting.enabled {
            "disabled"
        } else if required > config.desired_replicas {
            "required_replicas_exceed_desired"
        } else {
            config.unavailable_reason.as_deref().unwrap_or(match state {
                "healthy" => "passed",
                "unhealthy" => "probe_failed",
                _ => "insufficient_fresh_observations",
            })
        };
        transaction.execute(sql("UPDATE runtime_health_environments SET revision_id=$2,state=$3,reason=$4,valid_until=$5,accounted_at=$6,accounted_excluded=$7,eligible_since=CASE WHEN revision_id IS DISTINCT FROM $2 THEN $6 ELSE eligible_since END WHERE environment_id=$1",vec![environment_id.into(),revision.id.into(),state.into(),reason.into(),valid_until.unwrap_or(accounting.now).into(),accounting.now.into(),excluded.into()])).await.map_err(|e| db_error("advance uptime accounting", e))?;
        transaction
            .commit()
            .await
            .map_err(|e| db_error("commit uptime accounting", e))?;
        Ok(())
    }
}

#[derive(FromQueryResult)]
struct EnvironmentView {
    sleeping: bool,
    preset: String,
    portless: Option<bool>,
    revision_id: Option<Uuid>,
    configuration: Option<serde_json::Value>,
    enabled: Option<bool>,
    paused: Option<bool>,
    required_healthy: Option<i32>,
    state: Option<String>,
    reason: Option<String>,
    valid_until: Option<DateTime<Utc>>,
    accounted_at: Option<DateTime<Utc>>,
    eligible_since: Option<DateTime<Utc>>,
    accounting_revision: Option<Uuid>,
    new_observation_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
}

impl RuntimeHealthService {
    pub async fn summary(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<RuntimeHealthSummary, RuntimeHealthError> {
        let view = EnvironmentView::find_by_statement(sql("SELECT e.sleeping,p.preset::text AS preset,(SELECT bool_and(c.container_port IS NULL) FROM deployment_containers c WHERE c.deployment_id=e.current_deployment_id AND c.deleted_at IS NULL) AS portless,r.id AS revision_id,r.configuration,h.enabled,h.paused,h.required_healthy,h.state,h.reason,h.valid_until,h.accounted_at,h.eligible_since,h.revision_id AS accounting_revision,(SELECT MIN(a.observed_at) FROM runtime_health_assignments a WHERE a.revision_id=h.revision_id AND a.observed_at>h.accounted_at) AS new_observation_at,clock_timestamp() AS now FROM environments e JOIN projects p ON p.id=e.project_id LEFT JOIN runtime_health_revisions r ON r.deployment_id=e.current_deployment_id LEFT JOIN runtime_health_environments h ON h.environment_id=e.id WHERE e.id=$1 AND e.project_id=$2 AND NOT p.is_deleted",vec![environment_id.into(),project_id.into()])).one(self.db.as_ref()).await.map_err(|e| db_error("read environment runtime health", e))?.ok_or(RuntimeHealthError::NotFound { project_id, environment_id })?;
        let configuration = view
            .configuration
            .map(serde_json::from_value::<EffectiveProbeRevision>)
            .transpose()
            .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
        let configured = configuration.is_some();
        let required = view
            .required_healthy
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or_else(|| {
                configuration
                    .as_ref()
                    .map_or(1, |config| config.desired_replicas)
            });
        let policy = RuntimeHealthPolicy {
            enabled: view.enabled.unwrap_or(true),
            paused: view.paused.unwrap_or(false),
            required_healthy: view.required_healthy,
        };
        let active = policy.enabled && !policy.paused && !view.sleeping;
        let fresh = view.valid_until.is_some_and(|until| until > view.now)
            && view.revision_id.is_some()
            && view.revision_id == view.accounting_revision;
        let mut state = if active && fresh {
            view.state.as_deref().unwrap_or("unknown")
        } else {
            "unknown"
        }
        .to_string();
        let mut reason = if view.preset == "docker-compose" {
            "compose_not_supported"
        } else if !configured {
            "redeploy_to_configure"
        } else if view.sleeping {
            "sleeping"
        } else if !policy.enabled {
            "disabled"
        } else if policy.paused {
            "paused"
        } else if let Some(reason) = configuration
            .as_ref()
            .and_then(|config| config.unavailable_reason.as_deref())
        {
            reason
        } else if !fresh {
            "insufficient_fresh_observations"
        } else {
            view.reason.as_deref().unwrap_or("not_checked")
        }
        .to_string();
        let mut replicas = Vec::new();
        if let Some(revision) = view.revision_id {
            let rows = self.db.query_all(sql("SELECT a.container_record_id,CASE WHEN a.valid_until>clock_timestamp() AND a.lease_until>clock_timestamp() AND c.deleted_at IS NULL AND c.node_id IS NOT DISTINCT FROM a.node_id THEN a.state ELSE 'unknown' END AS state,CASE WHEN a.state='unknown' OR (a.valid_until>clock_timestamp() AND a.lease_until>clock_timestamp()) THEN a.reason ELSE 'stale' END AS reason,a.observed_at FROM runtime_health_assignments a JOIN deployment_containers c ON c.id=a.container_record_id WHERE a.revision_id=$1 AND a.retired_at IS NULL ORDER BY a.container_record_id LIMIT 1000",vec![revision.into()])).await.map_err(|e| db_error("read runtime replicas", e))?;
            for row in rows {
                replicas.push(RuntimeReplicaHealth {
                    replica_id: row
                        .try_get("", "container_record_id")
                        .map_err(|e| db_error("decode replica identity", e))?,
                    state: if active {
                        row.try_get("", "state")
                            .map_err(|e| db_error("decode replica state", e))?
                    } else {
                        "unknown".into()
                    },
                    reason: if active {
                        row.try_get("", "reason")
                            .map_err(|e| db_error("decode replica reason", e))?
                    } else {
                        reason.clone()
                    },
                    checked_at: row
                        .try_get("", "observed_at")
                        .map_err(|e| db_error("decode replica time", e))?,
                });
            }
        }
        if let Some(config) = &configuration {
            if active && config.unavailable_reason.is_none() {
                state = aggregate_state(
                    config.desired_replicas,
                    required,
                    replicas
                        .iter()
                        .filter(|replica| replica.state == "healthy")
                        .count() as u32,
                    replicas
                        .iter()
                        .filter(|replica| replica.state == "unhealthy")
                        .count() as u32,
                )
                .to_string();
                if matches!(
                    reason.as_str(),
                    "passed" | "probe_failed" | "insufficient_fresh_observations"
                ) {
                    reason = match state.as_str() {
                        "healthy" => "passed",
                        "unhealthy" => "probe_failed",
                        _ => "insufficient_fresh_observations",
                    }
                    .into();
                }
            }
        }
        let mut history = Vec::new();
        let mut healthy = 0.0;
        let mut unhealthy = 0.0;
        let mut excluded = 0.0;
        let mut total = 0.0;
        if configured {
            if let Some(eligible_since) = view
                .eligible_since
                .filter(|_| view.revision_id == view.accounting_revision)
            {
                // Exact whole-bucket start keeps coverage math independent of
                // dashboard polling time. The final live interval is projected
                // on reads, so a dead coordinator can never preserve green.
                let rows = self.db.query_all(sql(r#"
WITH bounds AS (
 SELECT date_bin(interval '1 minute',$2::timestamptz,'2000-01-01'::timestamptz)-interval '24 hours' AS start_at,$2::timestamptz AS end_at
)
SELECT b AS bucket_at,
 GREATEST(0,EXTRACT(epoch FROM LEAST(b+interval '1 minute',end_at)-GREATEST(b,start_at,$3::timestamptz)))::double precision AS total,
 COALESCE(h.healthy_seconds,0)::double precision AS healthy,
 COALESCE(h.unhealthy_seconds,0)::double precision AS unhealthy,
 COALESCE(h.excluded_seconds,0)::double precision AS excluded
FROM bounds,LATERAL generate_series(start_at,end_at,interval '1 minute') b
LEFT JOIN runtime_health_buckets h ON h.environment_id=$1 AND h.revision_id=$4 AND h.bucket_at=b
ORDER BY b
"#,vec![environment_id.into(),view.now.into(),eligible_since.into(),view.revision_id.into()])).await.map_err(|e| db_error("read uptime buckets", e))?;
                for row in rows {
                    let bucket_at: DateTime<Utc> = row
                        .try_get("", "bucket_at")
                        .map_err(|e| db_error("decode bucket time", e))?;
                    let duration: f64 = row
                        .try_get("", "total")
                        .map_err(|e| db_error("decode bucket duration", e))?;
                    if duration == 0.0 {
                        continue;
                    }
                    let mut h: f64 = row
                        .try_get("", "healthy")
                        .map_err(|e| db_error("decode healthy duration", e))?;
                    let mut u: f64 = row
                        .try_get("", "unhealthy")
                        .map_err(|e| db_error("decode unhealthy duration", e))?;
                    let mut x: f64 = row
                        .try_get("", "excluded")
                        .map_err(|e| db_error("decode excluded duration", e))?;
                    if let Some(accounted_at) = view.accounted_at {
                        let from = accounted_at.max(bucket_at).max(eligible_since);
                        let until = view.now.min(bucket_at + chrono::Duration::minutes(1));
                        let remaining = (until - from).num_milliseconds().max(0) as f64 / 1000.0;
                        if !active {
                            x += remaining;
                        } else if view.revision_id == view.accounting_revision {
                            let known_until = until
                                .min(view.valid_until.unwrap_or(from))
                                .min(view.new_observation_at.unwrap_or(until));
                            let known =
                                (known_until - from).num_milliseconds().max(0) as f64 / 1000.0;
                            match view.state.as_deref() {
                                Some("healthy") => h += known,
                                Some("unhealthy") => u += known,
                                _ => {}
                            }
                        }
                    }
                    // Float rounding must not produce negative unknown time.
                    h = h.min(duration);
                    u = u.min((duration - h).max(0.0));
                    x = x.min((duration - h - u).max(0.0));
                    let unknown = (duration - h - u - x).max(0.0);
                    healthy += h;
                    unhealthy += u;
                    excluded += x;
                    total += duration;
                    history.push(RuntimeHealthBucket {
                        bucket_at,
                        coverage: RuntimeCoverage::new(h, u, unknown, x),
                    });
                }
            }
        }
        Ok(RuntimeHealthSummary {
            environment_id,
            configured,
            portless: view.portless,
            state,
            reason,
            probe: configuration.map(|config| config.display(required)),
            policy,
            coverage: RuntimeCoverage::new(
                healthy,
                unhealthy,
                (total - healthy - unhealthy - excluded).max(0.0),
                excluded,
            ),
            replicas,
            history,
        })
    }

    pub async fn update_policy(
        &self,
        project_id: i32,
        environment_id: i32,
        policy: RuntimeHealthPolicy,
    ) -> Result<(), RuntimeHealthError> {
        if policy
            .required_healthy
            .is_some_and(|count| !(1..=1000).contains(&count))
        {
            return Err(RuntimeHealthError::Configuration { deployment_id: 0 });
        }
        let summary = self.summary(project_id, environment_id).await?;
        if policy.required_healthy.is_some_and(|required| {
            summary
                .probe
                .as_ref()
                .is_some_and(|probe| required as u32 > probe.desired_replicas)
        }) {
            return Err(RuntimeHealthError::Configuration { deployment_id: 0 });
        }
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|e| db_error("begin runtime policy update", e))?;
        transaction.execute(sql("INSERT INTO runtime_health_environments(environment_id) VALUES($1) ON CONFLICT(environment_id) DO NOTHING",vec![environment_id.into()])).await.map_err(|e| db_error("initialize runtime policy",e))?;
        let accounting=Accounting::find_by_statement(sql("SELECT h.*,clock_timestamp() AS now,(SELECT MIN(a.observed_at) FROM runtime_health_assignments a WHERE a.revision_id=h.revision_id AND a.observed_at>h.accounted_at) AS new_observation_at FROM runtime_health_environments h WHERE h.environment_id=$1 FOR UPDATE OF h",vec![environment_id.into()])).one(&transaction).await.map_err(|e| db_error("lock runtime policy",e))?.ok_or(RuntimeHealthError::NotFound { project_id,environment_id })?;
        persist_coverage(
            &transaction,
            environment_id,
            &accounting,
            accounting.valid_until,
        )
        .await?;
        transaction.execute(sql("UPDATE runtime_health_environments SET enabled=$2,paused=$3,required_healthy=$4,state='unknown',reason='policy_changed',valid_until=$5,accounted_at=$5,accounted_excluded=($3 OR NOT $2 OR (SELECT sleeping FROM environments WHERE id=$1)) WHERE environment_id=$1",vec![environment_id.into(),policy.enabled.into(),policy.paused.into(),policy.required_healthy.into(),accounting.now.into()])).await.map_err(|e| db_error("update runtime monitoring policy",e))?;
        // Keep an inactive lease until the node acknowledges revocation. On
        // resume, expire its DB authority so the next grant must use a higher
        // epoch and a new observation; a queued pre-pause success cannot revive
        // current health or its alert streaks.
        transaction.execute(sql("UPDATE runtime_health_assignments SET next_reconcile_at=clock_timestamp(),valid_until=clock_timestamp(),state='unknown',reason='policy_changed',failure_streak=0,recovery_streak=0,unknown_since=clock_timestamp(),lease_until=CASE WHEN $2 AND NOT $3 THEN clock_timestamp() ELSE lease_until END WHERE revision_id IN (SELECT id FROM runtime_health_revisions WHERE environment_id=$1) AND retired_at IS NULL",vec![environment_id.into(),policy.enabled.into(),policy.paused.into()])).await.map_err(|e| db_error("fence policy reconciliation",e))?;
        transaction
            .commit()
            .await
            .map_err(|e| db_error("commit runtime policy update", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::{MigrationTrait, SchemaManager};
    use temps_runtime_health::{HealthReason, HealthState, Observation};

    struct NoWorkers;
    #[async_trait::async_trait]
    impl RuntimeHealthTransportResolver for NoWorkers {
        async fn worker(
            &self,
            node_id: i32,
        ) -> Result<Arc<temps_deployer::remote::RemoteNodeDeployer>, RuntimeHealthError> {
            Err(RuntimeHealthError::Transport {
                node_id: Some(node_id),
            })
        }
    }

    async fn setup() -> Result<
        (
            temps_database::test_utils::TestDatabase,
            RuntimeHealthService,
        ),
        Box<dyn std::error::Error>,
    > {
        let database = temps_database::test_utils::TestDatabase::new().await?;
        let db = database.connection_arc();
        db.execute_unprepared("CREATE TABLE projects(id INTEGER PRIMARY KEY,preset TEXT NOT NULL,is_deleted BOOLEAN NOT NULL DEFAULT FALSE); CREATE TABLE environments(id INTEGER PRIMARY KEY,project_id INTEGER NOT NULL,current_deployment_id INTEGER,sleeping BOOLEAN NOT NULL DEFAULT FALSE,deleted_at TIMESTAMPTZ); CREATE TABLE deployments(id INTEGER PRIMARY KEY,project_id INTEGER NOT NULL,environment_id INTEGER NOT NULL,state TEXT NOT NULL); CREATE TABLE nodes(id INTEGER PRIMARY KEY); CREATE TABLE alarms(id INTEGER PRIMARY KEY,alarm_type TEXT,status TEXT,metadata JSONB,project_id INTEGER); CREATE TABLE deployment_containers(id INTEGER PRIMARY KEY,deployment_id INTEGER NOT NULL,container_id TEXT NOT NULL,container_port INTEGER,node_id INTEGER,deleted_at TIMESTAMPTZ); INSERT INTO projects(id,preset) VALUES(1,'dockerfile'); INSERT INTO environments(id,project_id,current_deployment_id) VALUES(2,1,3); INSERT INTO deployments VALUES(3,1,2,'completed'); INSERT INTO nodes VALUES(5),(6);").await?;
        db.execute(sql("INSERT INTO deployment_containers(id,deployment_id,container_id,container_port) VALUES(4,3,$1,NULL)",vec!["a".repeat(64).into()])).await?;
        temps_migrations::RuntimeHealthMigration
            .up(&SchemaManager::new(db.as_ref()))
            .await?;
        let service = RuntimeHealthService::new(
            db,
            Arc::new(NoWorkers),
            Arc::new(DockerHandle::disabled(
                "control-plane",
                "test has no Docker socket",
            )),
        );
        Ok((database, service))
    }
    fn config(replicas: u32) -> EffectiveProbeRevision {
        EffectiveProbeRevision::resolve(
            &HealthCheckConfig::default(),
            None,
            None,
            "automatic",
            replicas,
        )
    }
    async fn save(service: &RuntimeHealthService) {
        save_revision(service.db.as_ref(), 1, 2, 3, config(1))
            .await
            .unwrap();
    }
    async fn placement(service: &RuntimeHealthService) -> Placement {
        Placement::find_by_statement(sql("SELECT a.*,clock_timestamp() AS loaded_at,r.project_id,r.environment_id,r.deployment_id,r.configuration,c.container_id,c.container_port,TRUE AS current,TRUE AS monitoring,TRUE AS terminal,TRUE AS accounting_due FROM runtime_health_assignments a JOIN runtime_health_revisions r ON r.id=a.revision_id JOIN deployment_containers c ON c.id=a.container_record_id",vec![])).one(service.db.as_ref()).await.unwrap().unwrap()
    }
    async fn grant(service: &RuntimeHealthService, row: &Placement, session: Uuid) -> i64 {
        let lease=service.db.query_one(sql("INSERT INTO runtime_health_coordinator(owner,expires_at) VALUES($1,clock_timestamp()+interval '1 hour') RETURNING term",vec![service.coordinator.into()])).await.unwrap().unwrap();
        service.db.execute(sql("UPDATE runtime_health_assignments SET session=$1,lease_until=clock_timestamp()+interval '2 minutes' WHERE id=$2",vec![session.into(),row.id.into()])).await.unwrap();
        lease.try_get("", "term").unwrap()
    }
    fn sample(row: &Placement, sequence: u64) -> TimedObservation {
        TimedObservation {
            observation: Observation {
                assignment_id: row.id,
                revision: row.revision_id,
                placement_generation: row.container_record_id as u64,
                epoch: row.epoch as u64,
                sequence,
                state: HealthState::Healthy,
                reason: HealthReason::Passed,
            },
            age_ms: 0,
        }
    }
    async fn count(service: &RuntimeHealthService, table: &str) -> i64 {
        // Table names are fixed test constants, never API input.
        service
            .db
            .query_one(sql(
                &format!("SELECT COUNT(*)::bigint AS count FROM {table}"),
                vec![],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "count")
            .unwrap()
    }

    #[tokio::test]
    async fn revisions_are_immutable_and_docker_free_startup_stays_unknown(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        assert_eq!(count(&service, "runtime_health_revisions").await, 0);
        let summary = service.summary(1, 2).await?;
        assert!(!summary.configured);
        assert_eq!(summary.coverage.availability_percent, None);
        save(&service).await;
        save_revision(service.db.as_ref(), 1, 2, 3, config(3)).await?;
        assert_eq!(count(&service, "runtime_health_revisions").await, 1);
        assert!(service
            .db
            .execute_unprepared("UPDATE runtime_health_revisions SET configuration='{}'")
            .await
            .is_err());
        service.reconcile().await?;
        let summary = service.summary(1, 2).await?;
        assert!(summary.configured);
        assert_eq!(summary.state, "unknown");
        assert_eq!(summary.probe.unwrap().desired_replicas, 1);
        assert_eq!(summary.coverage.availability_percent, None);
        assert_eq!(summary.coverage.coverage_percent, Some(0.0));
        assert_eq!(count(&service, "runtime_health_samples").await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn acceptance_rejects_replays_and_stale_placement_revision_epoch_and_leader(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.accept(&row, session, sample(&row, 1), term).await?;
        service.accept(&row, session, sample(&row, 1), term).await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        let mut stale = sample(&row, 2);
        stale.observation.epoch += 1;
        service.accept(&row, session, stale, term).await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        let mut wrong_revision = sample(&row, 2);
        wrong_revision.observation.revision = Uuid::new_v4();
        assert!(matches!(
            service.accept(&row, session, wrong_revision, term).await,
            Err(RuntimeHealthError::StaleReport)
        ));
        let mut delayed = row.clone();
        delayed.loaded_at -= chrono::Duration::minutes(5);
        assert!(
            !service
                .accept(&delayed, session, sample(&delayed, 2), term)
                .await?
        );
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        let mut aged = sample(&row, 2);
        aged.age_ms = 100_000;
        assert!(matches!(
            service.accept(&row, session, aged, term).await,
            Err(RuntimeHealthError::StaleReport)
        ));
        service
            .accept(&row, Uuid::new_v4(), sample(&row, 2), term)
            .await?;
        service
            .accept(&row, session, sample(&row, 2), term + 1)
            .await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        service
            .db
            .execute_unprepared("UPDATE deployment_containers SET node_id=5 WHERE id=4")
            .await?;
        service.accept(&row, session, sample(&row, 2), term).await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        service.db.execute_unprepared("UPDATE deployment_containers SET node_id=NULL WHERE id=4; UPDATE environments SET current_deployment_id=NULL WHERE id=2").await?;
        service.accept(&row, session, sample(&row, 2), term).await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn report_batches_are_atomic_replay_safe_and_reject_duplicate_replicas(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        service.db.execute(sql("INSERT INTO deployment_containers(id,deployment_id,container_id,container_port) VALUES(8,3,$1,NULL)",vec!["b".repeat(64).into()])).await?;
        save(&service).await;
        let (term, rows) = service.take_due(Vec::new(), BATCH).await?.unwrap();
        assert_eq!(rows.len(), 2);
        let session = Uuid::new_v4();
        service.db.execute(sql("UPDATE runtime_health_assignments SET session=$1,lease_until=clock_timestamp()+interval '2 minutes'",vec![session.into()])).await?;
        let accepted = service
            .accept_batch(
                &rows,
                session,
                rows.iter().map(|row| sample(row, 1)).collect(),
                term,
            )
            .await?;
        assert_eq!(accepted.len(), 2);
        assert_eq!(count(&service, "runtime_health_samples").await, 2);
        assert!(service
            .accept_batch(
                &rows,
                session,
                rows.iter().map(|row| sample(row, 1)).collect(),
                term
            )
            .await?
            .is_empty());
        let mut invalid = sample(&rows[1], 2);
        invalid.observation.revision = Uuid::new_v4();
        assert!(matches!(
            service
                .accept_batch(&rows, session, vec![sample(&rows[0], 2), invalid], term)
                .await,
            Err(RuntimeHealthError::StaleReport)
        ));
        assert!(matches!(
            service
                .accept_batch(
                    &rows,
                    session,
                    vec![sample(&rows[0], 2), sample(&rows[0], 3)],
                    term
                )
                .await,
            Err(RuntimeHealthError::StaleReport)
        ));
        assert_eq!(count(&service, "runtime_health_samples").await, 2);
        service
            .db
            .execute_unprepared("UPDATE projects SET is_deleted=TRUE WHERE id=1")
            .await?;
        assert!(service
            .accept_batch(
                &rows,
                session,
                rows.iter().map(|row| sample(row, 2)).collect(),
                term
            )
            .await?
            .is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn slow_probe_intervals_keep_sample_freshness_separate_from_execution_leases(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        let mut configuration = config(1);
        configuration.interval_seconds = 300;
        save_revision(service.db.as_ref(), 1, 2, 3, configuration).await?;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.accept(&row, session, sample(&row, 1), term).await?;
        let nominal: bool = service
            .db
            .query_one(sql(
                "SELECT valid_until>lease_until AS independent FROM runtime_health_assignments",
                vec![],
            ))
            .await?
            .unwrap()
            .try_get("", "independent")?;
        assert!(nominal);
        service.account_environment(2, term).await?;
        assert_eq!(service.summary(1, 2).await?.state, "healthy");
        // Renewed authority can preserve this still-fresh sample without
        // inventing a new observation or incrementing alert counters.
        service.db.execute_unprepared("UPDATE runtime_health_assignments SET lease_until=clock_timestamp()+interval '1 hour'; UPDATE runtime_health_environments SET valid_until=clock_timestamp()-interval '1 second'").await?;
        service.account_environment(2, term).await?;
        assert_eq!(service.summary(1, 2).await?.state, "healthy");
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        service.db.execute_unprepared("UPDATE runtime_health_assignments SET lease_until=clock_timestamp()-interval '1 second'").await?;
        assert_eq!(service.summary(1, 2).await?.state, "unknown");
        Ok(())
    }

    #[tokio::test]
    async fn coverage_caps_success_at_freshness_and_does_not_backfill_downtime(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.db.execute(sql("INSERT INTO runtime_health_environments(environment_id,revision_id,state,valid_until,accounted_at,eligible_since) VALUES(2,$1,'healthy',clock_timestamp()-interval '10 seconds',clock_timestamp()-interval '20 seconds',clock_timestamp()-interval '20 seconds')",vec![row.revision_id.into()])).await?;
        service.account_environment(2, term).await?;
        let summary = service.summary(1, 2).await?;
        assert_eq!(summary.state, "unknown");
        assert!((summary.coverage.healthy_seconds - 10.0).abs() < 0.2);
        assert!(summary.coverage.unknown_seconds >= 10.0);
        assert_eq!(summary.coverage.availability_percent, Some(100.0));
        assert!(summary.coverage.coverage_percent.unwrap() < 51.0);
        service.account_environment(2, term).await?;
        let again = service.summary(1, 2).await?;
        assert!((again.coverage.healthy_seconds - summary.coverage.healthy_seconds).abs() < 0.01);
        service.accept(&row, session, sample(&row, 1), term).await?;
        service.account_environment(2, term).await?;
        assert_eq!(service.summary(1, 2).await?.state, "healthy");
        Ok(())
    }

    #[tokio::test]
    async fn reads_expire_without_a_coordinator_and_placement_changes_invalidate_green(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.db.execute(sql("INSERT INTO runtime_health_environments(environment_id,revision_id,state,valid_until,accounted_at,eligible_since) VALUES(2,$1,'healthy',clock_timestamp()-interval '10 seconds',clock_timestamp()-interval '20 seconds',clock_timestamp()-interval '20 seconds')",vec![row.revision_id.into()])).await?;
        let expired = service.summary(1, 2).await?;
        assert_eq!(expired.state, "unknown");
        assert!((expired.coverage.healthy_seconds - 10.0).abs() < 0.2);
        assert!(expired.coverage.unknown_seconds >= 10.0);
        service.accept(&row, session, sample(&row, 1), term).await?;
        service.account_environment(2, term).await?;
        assert_eq!(service.summary(1, 2).await?.state, "healthy");
        service
            .db
            .execute_unprepared("UPDATE deployment_containers SET node_id=5 WHERE id=4")
            .await?;
        assert_eq!(service.summary(1, 2).await?.state, "unknown");
        service
            .db
            .execute_unprepared("UPDATE environments SET current_deployment_id=NULL WHERE id=2")
            .await?;
        service.account_environment(2, term).await?;
        let known: i64 = service.db.query_one(sql("SELECT COUNT(*)::bigint AS count FROM runtime_health_environments WHERE state IN ('healthy','unhealthy')",vec![])).await?.unwrap().try_get("", "count")?;
        assert_eq!(known, 0);
        Ok(())
    }

    #[tokio::test]
    async fn workload_metadata_distinguishes_workers_web_and_undeployed_environments(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        let metadata = service.workloads(1).await?;
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].portless, Some(true));
        assert!(!metadata[0].configured);
        save(&service).await;
        assert!(service.workloads(1).await?[0].configured);
        service
            .db
            .execute_unprepared("UPDATE deployment_containers SET container_port=8080 WHERE id=4")
            .await?;
        assert_eq!(service.workloads(1).await?[0].portless, Some(false));
        service
            .db
            .execute_unprepared("UPDATE environments SET current_deployment_id=NULL WHERE id=2")
            .await?;
        assert_eq!(service.workloads(1).await?[0].portless, None);
        assert!(matches!(
            service.workloads(99).await,
            Err(RuntimeHealthError::NotFound { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn pending_revisions_are_not_retired_or_probed_before_completion(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        service
            .db
            .execute_unprepared("UPDATE deployments SET state='running'")
            .await?;
        save(&service).await;
        service.reconcile().await?;
        let pending = service
            .db
            .query_one(sql(
                "SELECT retired_at,activated_at,lease_until FROM runtime_health_assignments",
                vec![],
            ))
            .await?
            .unwrap();
        assert_eq!(
            pending.try_get::<Option<DateTime<Utc>>>("", "retired_at")?,
            None
        );
        assert_eq!(
            pending.try_get::<Option<DateTime<Utc>>>("", "activated_at")?,
            None
        );
        assert_eq!(
            pending.try_get::<Option<DateTime<Utc>>>("", "lease_until")?,
            None
        );
        service.db.execute_unprepared("UPDATE deployments SET state='completed'; UPDATE runtime_health_assignments SET next_reconcile_at=clock_timestamp()").await?;
        service.reconcile().await?;
        let activated = service
            .db
            .query_one(sql(
                "SELECT activated_at FROM runtime_health_assignments",
                vec![],
            ))
            .await?
            .unwrap();
        assert!(activated
            .try_get::<Option<DateTime<Utc>>>("", "activated_at")?
            .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn an_unaggregated_failure_or_removed_assignment_cannot_extend_previous_green_coverage(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.db.execute(sql("INSERT INTO runtime_health_environments(environment_id,revision_id,state,valid_until,accounted_at,eligible_since) VALUES(2,$1,'healthy',clock_timestamp()+interval '1 minute',clock_timestamp()-interval '5 seconds',clock_timestamp()-interval '5 seconds')",vec![row.revision_id.into()])).await?;
        let mut failure = sample(&row, 1);
        failure.observation.state = HealthState::Unhealthy;
        failure.observation.reason = HealthReason::NotRunning;
        service.accept(&row, session, failure, term).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let pending = service.summary(1, 2).await?;
        assert_eq!(pending.state, "unhealthy");
        assert!((pending.coverage.healthy_seconds - 5.0).abs() < 0.08);
        assert!(pending.coverage.unknown_seconds > 0.08);
        service
            .db
            .execute_unprepared("DELETE FROM runtime_health_assignments")
            .await?;
        let removed = service.summary(1, 2).await?;
        assert_eq!(removed.state, "unknown");
        assert!((removed.coverage.healthy_seconds - pending.coverage.healthy_seconds).abs() < 0.01);
        service.account_environment(2, term).await?;
        let accounted = service.summary(1, 2).await?;
        assert!(
            (accounted.coverage.healthy_seconds - pending.coverage.healthy_seconds).abs() < 0.01
        );
        Ok(())
    }

    #[tokio::test]
    async fn deployment_revisions_do_not_inherit_previous_uptime(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let term = grant(&service, &row, Uuid::new_v4()).await;
        service.db.execute(sql("INSERT INTO runtime_health_environments(environment_id,revision_id,state,valid_until,accounted_at,eligible_since) VALUES(2,$1,'healthy',clock_timestamp()+interval '1 minute',clock_timestamp()-interval '20 seconds',clock_timestamp()-interval '20 seconds')",vec![row.revision_id.into()])).await?;
        service.account_environment(2, term).await?;
        assert!(service.summary(1, 2).await?.coverage.healthy_seconds >= 19.0);
        service.db.execute_unprepared("INSERT INTO deployments VALUES(7,1,2,'completed'); UPDATE environments SET current_deployment_id=7 WHERE id=2").await?;
        save_revision(service.db.as_ref(), 1, 2, 7, config(1)).await?;
        service.account_environment(2, term).await?;
        let summary = service.summary(1, 2).await?;
        assert_eq!(summary.state, "unknown");
        assert_eq!(summary.coverage.healthy_seconds, 0.0);
        assert_eq!(summary.coverage.availability_percent, None);
        assert!(count(&service, "runtime_health_buckets").await > 0);
        Ok(())
    }

    #[test]
    fn short_observed_intervals_keep_percentages_within_bounds() {
        for milliseconds in 1..=10_000 {
            let healthy = milliseconds as f64 / 1000.0;
            let coverage = RuntimeCoverage::new(healthy, 0.0, 0.0, 0.0);
            assert_eq!(coverage.availability_percent, Some(100.0));
            assert_eq!(coverage.coverage_percent, Some(100.0));
        }
    }

    #[tokio::test]
    async fn fresh_healthy_replicas_do_not_invent_uptime_before_accounting(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        assert!(service.accept(&row, session, sample(&row, 1), term).await?);

        let summary = service.summary(1, 2).await?;
        assert_eq!(summary.state, "healthy");
        assert_eq!(summary.coverage.healthy_seconds, 0.0);
        assert_eq!(summary.coverage.availability_percent, None);
        Ok(())
    }

    #[test]
    fn replica_quorum_keeps_missing_slots_unknown() {
        assert_eq!(aggregate_state(3, 3, 1, 0), "unknown");
        assert_eq!(aggregate_state(3, 3, 1, 1), "unhealthy");
        assert_eq!(aggregate_state(3, 2, 2, 1), "healthy");
        assert_eq!(aggregate_state(3, 2, 1, 1), "unknown");
        assert_eq!(aggregate_state(3, 2, 0, 2), "unhealthy");
        assert_eq!(aggregate_state(3, 4, 3, 0), "unknown");
        assert_eq!(
            RuntimeCoverage::new(0.0, 0.0, 100.0, 0.0).availability_percent,
            None
        );
        assert_eq!(
            RuntimeCoverage::new(50.0, 50.0, 100.0, 0.0).coverage_percent,
            Some(50.0)
        );
    }

    #[test]
    fn exec_is_unavailable_and_effective_configuration_never_contains_argv_or_query_secrets() {
        let health = HealthCheckConfig {
            probe_type: Some("exec".into()),
            command: vec!["sh".into(), "-c".into(), "private-value".into()],
            ..Default::default()
        };
        let config = EffectiveProbeRevision::resolve(&health, None, None, "repository", 1);
        assert_eq!(
            config.unavailable_reason.as_deref(),
            Some("runtime_exec_unavailable")
        );
        assert!(!serde_json::to_string(&config)
            .unwrap()
            .contains("private-value"));
        let http = HealthCheckConfig {
            path: "/ready?token=private-value".into(),
            ..Default::default()
        };
        let display =
            EffectiveProbeRevision::resolve(&http, Some(8080), None, "repository", 1).display(1);
        assert!(!serde_json::to_string(&display)
            .unwrap()
            .contains("private-value"));
    }
    #[tokio::test]
    async fn pause_closes_prior_coverage_and_rejects_fresh_samples_until_resumed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.accept(&row, session, sample(&row, 1), term).await?;
        service.db.execute(sql("INSERT INTO runtime_health_environments(environment_id,revision_id,state,valid_until,accounted_at,eligible_since) VALUES(2,$1,'healthy',clock_timestamp()+interval '1 minute',clock_timestamp()-interval '5 seconds',clock_timestamp()-interval '5 seconds')",vec![row.revision_id.into()])).await?;
        service
            .update_policy(
                1,
                2,
                RuntimeHealthPolicy {
                    enabled: true,
                    paused: true,
                    required_healthy: None,
                },
            )
            .await?;
        service.accept(&row, session, sample(&row, 2), term).await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let paused = service.summary(1, 2).await?;
        assert_eq!(paused.reason, "paused");
        assert!((paused.coverage.healthy_seconds - 5.0).abs() < 0.2);
        assert!(paused.coverage.excluded_seconds > 0.08);
        service
            .update_policy(
                1,
                2,
                RuntimeHealthPolicy {
                    enabled: true,
                    paused: false,
                    required_healthy: None,
                },
            )
            .await?;
        assert_eq!(service.summary(1, 2).await?.state, "unknown");
        service.accept(&row, session, sample(&row, 2), term).await?;
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        assert_eq!(service.summary(1, 2).await?.state, "unknown");
        assert!(service.summary(1, 2).await?.coverage.excluded_seconds > 0.08);
        assert!(service
            .update_policy(
                1,
                2,
                RuntimeHealthPolicy {
                    enabled: true,
                    paused: false,
                    required_healthy: Some(2)
                }
            )
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn repeated_authorization_rejection_preserves_loss_start_and_raises_one_alarm(
    ) -> Result<(), Box<dyn std::error::Error>> {
        struct RejectingInspector;
        #[async_trait::async_trait]
        impl temps_runtime_health::RuntimeInspector for RejectingInspector {
            fn claim(
                &self,
                _assignment: &Assignment,
            ) -> Result<
                Box<dyn temps_runtime_health::ProbePermit>,
                temps_runtime_health::InspectFailure,
            > {
                Err(temps_runtime_health::InspectFailure::Unauthorized)
            }
            async fn inspect(
                &self,
                _assignment: &Assignment,
            ) -> Result<temps_runtime_health::RuntimeSnapshot, temps_runtime_health::InspectFailure>
            {
                Err(temps_runtime_health::InspectFailure::Unauthorized)
            }
        }
        let (_database, service) = setup().await?;
        save(&service).await;
        let directory = tempfile::tempdir()?;
        let host = Arc::new(ProbeHost::open(
            ExecutionOwner::Embedded(Uuid::new_v4()),
            directory.path().join("journal"),
            Arc::new(RejectingInspector),
        )?);
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.accept(&row, session, sample(&row, 1), term).await?;
        service.account_environment(2, term).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(service.embedded.set(host).is_ok());
        let mut changed = std::collections::BTreeSet::new();
        service
            .reconcile_node(None, vec![row.clone()], term, &mut changed)
            .await?;
        let rejected = service.summary(1, 2).await?;
        assert_eq!(rejected.state, "unknown");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            (service.summary(1, 2).await?.coverage.healthy_seconds
                - rejected.coverage.healthy_seconds)
                .abs()
                < 0.01
        );
        assert!(service
            .db
            .query_one(sql(
                "SELECT unknown_since FROM runtime_health_assignments",
                vec![]
            ))
            .await?
            .unwrap()
            .try_get::<Option<DateTime<Utc>>>("", "unknown_since")?
            .is_some());
        service.stage_alarms(vec![row.id], term).await?;
        assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 0);
        service.db.execute_unprepared("UPDATE runtime_health_assignments SET unknown_since=clock_timestamp()-interval '121 seconds'").await?;
        let since: DateTime<Utc> = service
            .db
            .query_one(sql(
                "SELECT unknown_since FROM runtime_health_assignments",
                vec![],
            ))
            .await?
            .unwrap()
            .try_get("", "unknown_since")?;
        for _ in 0..3 {
            service
                .reconcile_node(None, vec![placement(&service).await], term, &mut changed)
                .await?;
            service.stage_alarms(vec![row.id], term).await?;
            let state = service.db.query_one(sql("SELECT unknown_since,reason,lease_until<=clock_timestamp() AS expired FROM runtime_health_assignments", vec![])).await?.unwrap();
            assert_eq!(state.try_get::<DateTime<Utc>>("", "unknown_since")?, since);
            assert_eq!(
                state.try_get::<String>("", "reason")?,
                "authorization_failed"
            );
            assert!(state.try_get::<bool>("", "expired")?);
            assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 1);
        }
        let event = service
            .db
            .query_one(sql(
                "SELECT event_type FROM runtime_health_alarm_outbox",
                vec![],
            ))
            .await?
            .unwrap();
        assert_eq!(event.try_get::<String>("", "event_type")?, "fire_lost");
        assert_eq!(changed, std::collections::BTreeSet::from([2]));
        assert_eq!(count(&service, "runtime_health_samples").await, 1);
        Ok(())
    }

    #[tokio::test]
    async fn alarm_thresholds_deduplicate_and_distinguish_monitoring_loss_from_failure(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_database, service) = setup().await?;
        save(&service).await;
        let row = placement(&service).await;
        let session = Uuid::new_v4();
        let term = grant(&service, &row, session).await;
        service.db.execute_unprepared("UPDATE runtime_health_assignments SET activated_at=clock_timestamp()-interval '5 minutes'").await?;
        for sequence in 1..=2 {
            let mut failure = sample(&row, sequence);
            failure.observation.state = HealthState::Unhealthy;
            failure.observation.reason = temps_runtime_health::HealthReason::NotRunning;
            service.accept(&row, session, failure, term).await?;
        }
        service.stage_alarms(vec![row.id], term).await?;
        assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 0);
        let mut failure = sample(&row, 3);
        failure.observation.state = HealthState::Unhealthy;
        failure.observation.reason = temps_runtime_health::HealthReason::NotRunning;
        service.accept(&row, session, failure.clone(), term).await?;
        service.stage_alarms(vec![row.id], term).await?;
        assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 1);
        service.accept(&row, session, failure, term).await?;
        service.stage_alarms(vec![row.id], term).await?;
        assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 1);
        service.accept(&row, session, sample(&row, 4), term).await?;
        service.stage_alarms(vec![row.id], term).await?;
        assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 1);
        service.accept(&row, session, sample(&row, 5), term).await?;
        service.stage_alarms(vec![row.id], term).await?;
        assert_eq!(count(&service, "runtime_health_alarm_outbox").await, 2);
        service.db.execute_unprepared("UPDATE runtime_health_assignments SET state='unknown',reason='runtime_unavailable',valid_until=clock_timestamp()-interval '121 seconds',unknown_since=clock_timestamp()-interval '121 seconds'").await?;
        service.stage_alarms(vec![row.id], term).await?;
        let lost=service.db.query_one(sql("SELECT event_type FROM runtime_health_alarm_outbox ORDER BY created_at DESC LIMIT 1",vec![])).await?.unwrap();
        assert_eq!(lost.try_get::<String>("", "event_type")?, "fire_lost");
        service.accept(&row, session, sample(&row, 6), term).await?;
        service.stage_alarms(vec![row.id], term).await?;
        let recovered=service.db.query_one(sql("SELECT event_type FROM runtime_health_alarm_outbox ORDER BY created_at DESC LIMIT 1",vec![])).await?.unwrap();
        assert_eq!(
            recovered.try_get::<String>("", "event_type")?,
            "resolve_lost"
        );
        Ok(())
    }
}

#[derive(FromQueryResult)]
struct AlarmCandidate {
    id: Uuid,
    configuration: serde_json::Value,
    state: String,
    fresh: bool,
    lost: bool,
    inactive: bool,
    failure_streak: i32,
    recovery_streak: i32,
    alarm_state: String,
    monitoring_lost: bool,
}

#[derive(FromQueryResult)]
struct AlarmDelivery {
    id: Uuid,
    assignment_id: Uuid,
    event_type: String,
    project_id: Option<i32>,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
    container_record_id: Option<i32>,
    current: bool,
    alarm_state: Option<String>,
    monitoring_lost: Option<bool>,
}

impl RuntimeHealthService {
    async fn stage_alarms(&self, ids: Vec<Uuid>, term: i64) -> Result<(), RuntimeHealthError> {
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|e| db_error("begin runtime alarm transitions", e))?;
        let leader=transaction.query_one(sql("SELECT term FROM runtime_health_coordinator WHERE owner=$1 AND term=$2 AND expires_at>clock_timestamp() FOR SHARE",vec![self.coordinator.into(),term.into()])).await.map_err(|e| db_error("fence runtime alarm transitions",e))?;
        if leader.is_none() {
            return Ok(());
        }
        let ids = serde_json::to_value(ids)
            .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
        let rows=AlarmCandidate::find_by_statement(sql("SELECT a.id,r.configuration,a.state,COALESCE(a.valid_until>clock_timestamp() AND a.lease_until>clock_timestamp(),FALSE) AS fresh,((a.state='unknown' AND a.reason IN ('runtime_unavailable','unauthorized','endpoint_unavailable','identity_mismatch','cancelled','authorization_failed') AND a.unknown_since<clock_timestamp()-interval '120 seconds') OR COALESCE(LEAST(a.valid_until,a.lease_until),a.activated_at,a.created_at)<clock_timestamp()-interval '120 seconds') AS lost,(a.retired_at IS NOT NULL OR e.current_deployment_id IS DISTINCT FROM r.deployment_id OR d.state NOT IN ('completed','deployed') OR c.deleted_at IS NOT NULL OR p.is_deleted OR NOT COALESCE(h.enabled,TRUE) OR COALESCE(h.paused,FALSE) OR e.sleeping OR r.configuration->>'unavailable_reason' IS NOT NULL) AS inactive,a.failure_streak,a.recovery_streak,a.alarm_state,a.monitoring_lost FROM runtime_health_assignments a JOIN runtime_health_revisions r ON r.id=a.revision_id JOIN deployment_containers c ON c.id=a.container_record_id JOIN environments e ON e.id=r.environment_id JOIN deployments d ON d.id=r.deployment_id JOIN projects p ON p.id=r.project_id LEFT JOIN runtime_health_environments h ON h.environment_id=e.id WHERE a.id IN (SELECT value::uuid FROM jsonb_array_elements_text($1)) ORDER BY a.id FOR UPDATE OF a",vec![ids.into()])).all(&transaction).await.map_err(|e| db_error("load runtime alarm transitions",e))?;
        for row in rows {
            let config: EffectiveProbeRevision = serde_json::from_value(row.configuration)
                .map_err(|_| RuntimeHealthError::Configuration { deployment_id: 0 })?;
            let mut probe = row.alarm_state.clone();
            let mut lost = row.monitoring_lost;
            let mut events = Vec::new();
            if row.inactive {
                if probe == "unhealthy" {
                    events.push("resolve_probe");
                    probe = "unknown".into();
                }
                if lost {
                    events.push("resolve_lost");
                    lost = false;
                }
            } else {
                if row.fresh
                    && row.state == "unhealthy"
                    && row.failure_streak as u32 >= config.failure_threshold
                    && probe != "unhealthy"
                {
                    events.push("fire_probe");
                    probe = "unhealthy".into();
                }
                if row.fresh
                    && row.state == "healthy"
                    && row.recovery_streak as u32 >= config.recovery_threshold
                    && probe != "healthy"
                {
                    events.push("resolve_probe");
                    probe = "healthy".into();
                }
                if row.lost && !lost {
                    events.push("fire_lost");
                    lost = true;
                }
                if row.fresh && !row.lost && lost {
                    events.push("resolve_lost");
                    lost = false;
                }
            }
            if events.is_empty() {
                continue;
            }
            for event in events {
                transaction.execute(sql("INSERT INTO runtime_health_alarm_outbox(id,assignment_id,event_type) VALUES($1,$2,$3)",vec![Uuid::new_v4().into(),row.id.into(),event.into()])).await.map_err(|e| db_error("enqueue runtime alarm transition",e))?;
            }
            transaction.execute(sql("UPDATE runtime_health_assignments SET alarm_state=$2,monitoring_lost=$3 WHERE id=$1",vec![row.id.into(),probe.into(),lost.into()])).await.map_err(|e| db_error("persist runtime alarm transition",e))?;
        }
        transaction
            .commit()
            .await
            .map_err(|e| db_error("commit runtime alarm transitions", e))?;
        Ok(())
    }

    async fn dispatch_alarms(&self, term: i64) -> Result<(), RuntimeHealthError> {
        use temps_monitoring::{AlarmSeverity, AlarmType, FireAlarmRequest};
        let Some(alarms) = self.alarms.get() else {
            return Ok(());
        };
        let events=AlarmDelivery::find_by_statement(sql("SELECT o.id,o.assignment_id,o.event_type,r.project_id,r.environment_id,r.deployment_id,a.container_record_id,COALESCE(a.retired_at IS NULL AND e.current_deployment_id=r.deployment_id AND d.state IN ('completed','deployed') AND c.deleted_at IS NULL AND NOT e.sleeping AND COALESCE(h.enabled,TRUE) AND NOT COALESCE(h.paused,FALSE),FALSE) AS current,a.alarm_state,a.monitoring_lost FROM runtime_health_alarm_outbox o LEFT JOIN runtime_health_assignments a ON a.id=o.assignment_id LEFT JOIN runtime_health_revisions r ON r.id=a.revision_id LEFT JOIN deployments d ON d.id=r.deployment_id LEFT JOIN environments e ON e.id=r.environment_id LEFT JOIN deployment_containers c ON c.id=a.container_record_id LEFT JOIN runtime_health_environments h ON h.environment_id=e.id WHERE o.delivered_at IS NULL AND o.ready_at<=clock_timestamp() AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$1 AND term=$2 AND expires_at>clock_timestamp()) ORDER BY o.ready_at,o.id LIMIT 100",vec![self.coordinator.into(),term.into()])).all(self.db.as_ref()).await.map_err(|e| db_error("load runtime alarm outbox",e))?;
        for event in events {
            let probe = event.event_type.ends_with("probe");
            let alarm_type = if probe {
                AlarmType::RuntimeProbeFailed
            } else {
                AlarmType::RuntimeMonitoringLost
            };
            let should_fire = event.current
                && if probe {
                    event.alarm_state.as_deref() == Some("unhealthy")
                } else {
                    event.monitoring_lost == Some(true)
                };
            let firing = event.event_type.starts_with("fire_");
            let result = if firing && should_fire {
                // A crash between insert and acknowledgement must not create a
                // second alarm for the same durable transition.
                let existing=self.db.query_one(sql("SELECT id FROM alarms WHERE alarm_type=$1 AND metadata->>'runtime_health_event'=$2 LIMIT 1",vec![alarm_type.as_str().into(),event.id.to_string().into()])).await.map_err(|e| db_error("deduplicate runtime alarm delivery",e))?;
                if existing.is_some() {
                    Ok(())
                } else {
                    alarms.fire_runtime_alarm(FireAlarmRequest { project_id:event.project_id,environment_id:event.environment_id,deployment_id:event.deployment_id,container_id:event.container_record_id,service_id:None,alarm_type,severity:if probe { AlarmSeverity::Critical } else { AlarmSeverity::Warning },title:if probe { "Runtime health probe failed" } else { "Runtime health monitoring lost" }.into(),message:if probe { "The assigned replica failed its authorized runtime probe for the configured failure threshold." } else { "Fresh authorized observations are unavailable. Application availability is unknown; the application has not been restarted or stopped." }.into(),metadata:Some(serde_json::json!({"runtime_health_assignment":event.assignment_id,"runtime_health_event":event.id})) },event.assignment_id,event.id,self.coordinator,term).await.map(|_|())
                }
            } else if !firing && !should_fire {
                alarms
                    .resolve_runtime_assignment(&event.assignment_id.to_string(), alarm_type)
                    .await
            } else {
                Ok(())
            };
            match result {
                Ok(()) => {
                    self.db.execute(sql("UPDATE runtime_health_alarm_outbox SET delivered_at=clock_timestamp() WHERE id=$1",vec![event.id.into()])).await.map_err(|e| db_error("acknowledge runtime alarm delivery",e))?;
                }
                Err(error) => {
                    tracing::warn!(event_id=%event.id,%error,"Runtime health alarm delivery will retry");
                    self.db.execute(sql("UPDATE runtime_health_alarm_outbox SET attempts=LEAST(attempts+1,1000),ready_at=clock_timestamp()+interval '30 seconds' WHERE id=$1",vec![event.id.into()])).await.map_err(|e| db_error("retry runtime alarm delivery",e))?;
                }
            }
        }
        Ok(())
    }

    async fn expire_environments(&self, term: i64) -> Result<(), RuntimeHealthError> {
        let rows=self.db.query_all(sql("SELECT environment_id FROM runtime_health_environments WHERE state IN ('healthy','unhealthy') AND valid_until<=clock_timestamp() ORDER BY valid_until,environment_id LIMIT 100",vec![])).await.map_err(|e|db_error("load expired runtime environments",e))?;
        for row in rows {
            let environment_id: i32 = row
                .try_get("", "environment_id")
                .map_err(|e| db_error("decode expired runtime environment", e))?;
            self.account_environment(environment_id, term).await?;
        }
        Ok(())
    }

    async fn prune_history(&self, term: i64) -> Result<(), RuntimeHealthError> {
        let mut next = self.maintenance.lock().await;
        if *next > tokio::time::Instant::now() {
            return Ok(());
        }
        *next = tokio::time::Instant::now() + Duration::from_secs(5);
        drop(next);
        for query in [
            "DELETE FROM runtime_health_samples WHERE (assignment_id,epoch,sequence) IN (SELECT assignment_id,epoch,sequence FROM runtime_health_samples WHERE received_at<clock_timestamp()-interval '7 days' ORDER BY received_at LIMIT 2000)",
            "DELETE FROM runtime_health_buckets WHERE (environment_id,revision_id,bucket_at) IN (SELECT environment_id,revision_id,bucket_at FROM runtime_health_buckets WHERE bucket_at<clock_timestamp()-interval '30 days' ORDER BY bucket_at LIMIT 2000)",
            "DELETE FROM runtime_health_alarm_outbox WHERE id IN (SELECT id FROM runtime_health_alarm_outbox WHERE delivered_at<clock_timestamp()-interval '7 days' ORDER BY delivered_at LIMIT 2000)",
        ] {
            self.db.execute(sql(&format!("{query} AND EXISTS(SELECT 1 FROM runtime_health_coordinator WHERE owner=$1 AND term=$2 AND expires_at>clock_timestamp())"),vec![self.coordinator.into(),term.into()])).await.map_err(|e| db_error("prune bounded runtime history",e))?;
        }
        if let Some(alarms) = self.alarms.get() {
            let orphans=self.db.query_all(sql("SELECT DISTINCT metadata->>'runtime_health_assignment' AS assignment_id,alarm_type FROM alarms WHERE alarm_type IN ('runtime_probe_failed','runtime_monitoring_lost') AND status IN ('firing','acknowledged') AND metadata->>'runtime_health_assignment' IS NOT NULL AND NOT EXISTS(SELECT 1 FROM runtime_health_assignments a JOIN runtime_health_revisions r ON r.id=a.revision_id JOIN environments e ON e.id=r.environment_id WHERE a.id::text=alarms.metadata->>'runtime_health_assignment' AND a.retired_at IS NULL AND e.current_deployment_id=r.deployment_id) LIMIT 100",vec![])).await.map_err(|e| db_error("load retired runtime alarms",e))?;
            for row in orphans {
                let assignment: String = row
                    .try_get("", "assignment_id")
                    .map_err(|e| db_error("decode retired alarm identity", e))?;
                let kind: String = row
                    .try_get("", "alarm_type")
                    .map_err(|e| db_error("decode retired alarm type", e))?;
                if let Some(kind) = temps_monitoring::AlarmType::parse_alarm_type(&kind) {
                    alarms
                        .resolve_runtime_assignment(&assignment, kind)
                        .await
                        .map_err(|source| RuntimeHealthError::Alarm { source })?;
                }
            }
        }
        Ok(())
    }
}

/// Small project-wide classification feed. The console requests detailed
/// history only for its selected environment, not for every preview at once.
#[derive(Debug, Serialize, ToSchema, FromQueryResult)]
pub struct RuntimeWorkloadMetadata {
    pub environment_id: i32,
    pub portless: Option<bool>,
    pub configured: bool,
}

impl RuntimeHealthService {
    pub async fn workloads(
        &self,
        project_id: i32,
    ) -> Result<Vec<RuntimeWorkloadMetadata>, RuntimeHealthError> {
        let project = self
            .db
            .query_one(sql(
                "SELECT id FROM projects WHERE id=$1 AND NOT is_deleted",
                vec![project_id.into()],
            ))
            .await
            .map_err(|e| db_error("verify runtime workload project", e))?;
        if project.is_none() {
            return Err(RuntimeHealthError::NotFound {
                project_id,
                environment_id: 0,
            });
        }
        RuntimeWorkloadMetadata::find_by_statement(sql("SELECT e.id AS environment_id,(SELECT bool_and(c.container_port IS NULL) FROM deployment_containers c WHERE c.deployment_id=e.current_deployment_id AND c.deleted_at IS NULL) AS portless,(r.id IS NOT NULL) AS configured FROM environments e LEFT JOIN runtime_health_revisions r ON r.deployment_id=e.current_deployment_id WHERE e.project_id=$1 AND e.deleted_at IS NULL ORDER BY e.id LIMIT 1000",vec![project_id.into()])).all(self.db.as_ref()).await.map_err(|e|db_error("list runtime workload metadata",e))
    }
}
