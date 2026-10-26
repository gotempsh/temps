// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Gathers the facts [`compute`](super::compute) and [`plan`](super::plan)
//! judge, stores plans, starts, resumes, reconciles and undoes runs, and
//! tells the IP access service how visitors arrive (ADR 064 D3, D4, D9).
//!
//! A plan is stored for [`PLAN_TTL`] with a fingerprint of what it depends
//! on; `apply` recomputes it and, when anything changed, creates nothing and
//! answers with the new plan ([`ReachabilityError::Stale`]).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde_json::Value;
use temps_config::ConfigService;
use temps_core::public_hostname::PublicHostnameStrategy;
use temps_core::ways_in::{
    only_private, private_allow_list_message, AddressWayIn, EnvironmentWaysIn, PrivateAllowList,
    WayInKind, WaysInError,
};
use temps_core::worker_ingress::{
    address_scope, effective_public_address, parse_reported_ip, private_address_id, AddressScope,
    ReportedInterface,
};
use temps_dns::services::provider_service::{
    AddManagedDomainRequest, CreateProviderRequest, UpdateManagedDomainRequest,
};
use temps_dns::{
    CloudflareCredentials, DnsProviderService, DnsProviderType, DnsRecordContent, DnsRecordRequest,
    DnsRecordType, ManagedDnsRecordService, OwnershipScope, ProviderCredentials,
};
use temps_domains::DomainService;
use temps_entities::{
    deployment_containers, dns_managed_record_states, domains, environment_domains, environments,
    ingress_connectors, node_public_ingress, nodes, project_custom_domains,
    project_ip_access_rules, projects, reachability_plans, reachability_runs,
};
use temps_projects::CustomDomainService;
use thiserror::Error;
use tracing::{info, warn};

use super::compute::{
    address_reachability, is_local_only, needs_lookup, overall_state, summary, AddressFacts,
    AddressInput, DirectIngress, InsideCheck, PrivateBinding, PrivateFacts, Resolution,
    TunnelState, PRIVATE_COMMAND,
};
use super::model::{
    AddressAudience, AddressReachability, AddressSource, ApplyRequest, Audience, DecideRequest,
    DnsRecordState, EnvironmentReachability, Intent, JournalEntry, PlanOptionKind, PlanRequest,
    PlannedRecord, PreviousChoice, ReachabilityPlan, ResumeRequest, RunResource, RunState, RunStep,
    RunStepStatus, RunView, StepKind,
};
use super::plan::{
    fingerprint, guess_zone, http01_direct, plan, planned_address, planned_record, private_worker,
    record_type_of, records_to_read, tunnel_domain_for, tunnel_route, unique_tunnel_name, zone_for,
    NamedId, PlanFacts, PrivateWorkerFact, ZoneFact, FIELD_ADDRESS, FIELD_DNS_PROVIDER,
    FIELD_TOKEN,
};
use super::run::{
    created, decide, execute, reconcile_journal, resume, undo, NewTunnel, RunInput, RunOps,
    RunRecord, RunSink, Stop, Timing, TunnelStatus, WriteError, ZoneState,
};
use super::token_check::{check_token, TokenCheckRequest, TokenCheckResult};
use crate::error::IngressError;
use crate::service::{
    CreateConnectorRequest, CreateError, HostnameMode, IngressConnectorService,
    SetHostnameChoiceRequest, TunnelMoveGuard,
};

/// The DNS controller stamped on records a run sets.
const DNS_CONTROLLER: &str = "reachability";
/// How long one DNS lookup may take before the address is `unknown`.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_PLANE: &str = "control-plane";
/// How long a stored plan can be applied.
pub const PLAN_TTL: chrono::Duration = chrono::Duration::minutes(15);
/// How often orphaned runs are reconciled and expired plans purged.
pub const RECONCILE_EVERY: Duration = Duration::from_secs(60);
const RECOVERING: &str = "Recovering an interrupted setup: Temps is checking what its unfinished \
                          steps made before it can be resumed or undone.";

#[derive(Debug, Error)]
pub enum ReachabilityError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Ingress(#[from] IngressError),
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("Could not read the settings: {0}")]
    Settings(String),
    /// The plan no longer matches the cluster: nothing was created, and
    /// `plan` is the current one to review.
    #[error("{reason}")]
    Stale {
        reason: String,
        plan: Box<ReachabilityPlan>,
    },
}

impl From<ReachabilityError> for temps_core::problemdetails::Problem {
    fn from(error: ReachabilityError) -> Self {
        use axum::http::StatusCode;
        let error = match error {
            ReachabilityError::Ingress(inner) => return inner.into(),
            ReachabilityError::Stale { reason, plan } => {
                let problem = temps_core::problemdetails::new(StatusCode::CONFLICT)
                    .with_title("The Plan Changed")
                    .with_detail(reason);
                return match serde_json::to_value(&*plan) {
                    Ok(plan) => problem.with_value("plan", plan),
                    Err(_) => problem,
                };
            }
            other => other,
        };
        let (status, title) = match &error {
            ReachabilityError::NotFound(_) => (StatusCode::NOT_FOUND, "Not Found"),
            ReachabilityError::Validation(_) => {
                (StatusCode::BAD_REQUEST, "Cannot Make It Reachable")
            }
            ReachabilityError::Conflict(_) => (StatusCode::CONFLICT, "Cannot Make It Reachable"),
            ReachabilityError::Ingress(_)
            | ReachabilityError::Stale { .. }
            | ReachabilityError::Database(_)
            | ReachabilityError::Settings(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Reachability Error")
            }
        };
        temps_core::problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

/// The services a run and the facts come from. Resolved once every plugin
/// has registered (the projects plugin loads after this one).
pub struct ReachabilityDeps {
    pub db: Arc<DatabaseConnection>,
    pub connectors: Arc<IngressConnectorService>,
    pub dns_providers: Arc<DnsProviderService>,
    pub records: Arc<ManagedDnsRecordService>,
    pub custom_domains: Arc<CustomDomainService>,
    pub domains: Arc<DomainService>,
    pub config: Arc<ConfigService>,
}

pub struct ReachabilityService {
    deps: Arc<ReachabilityDeps>,
    http: reqwest::Client,
    /// Environments with a run in progress: one at a time each.
    active: Arc<Mutex<HashSet<i32>>>,
    timing: Timing,
}

/// The environment and project a request is about.
struct Scope {
    project: projects::Model,
    environment: environments::Model,
}

/// What the service knows about an environment's addresses.
struct Gathered {
    addresses: Vec<AddressInput>,
    running_on: Vec<String>,
    deployed: bool,
}

impl ReachabilityService {
    pub fn new(deps: ReachabilityDeps) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self {
            deps: Arc::new(deps),
            http,
            active: Arc::new(Mutex::new(HashSet::new())),
            timing: Timing::default(),
        }
    }

    fn ops(&self, project_id: Option<i32>, environment_id: Option<i32>) -> LiveOps {
        LiveOps {
            deps: self.deps.clone(),
            http: self.http.clone(),
            project_id,
            environment_id,
        }
    }

    fn owned(&self, environment_id: Option<i32>) -> bool {
        match (environment_id, self.active.lock()) {
            (Some(id), Ok(active)) => active.contains(&id),
            _ => false,
        }
    }

    /// Reconcile now, then every [`RECONCILE_EVERY`].
    pub fn start_reconciler(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                self.reconcile().await;
                tokio::time::sleep(RECONCILE_EVERY).await;
            }
        });
    }

    /// A run no task of this process is carrying (Temps restarted during
    /// it) cannot finish by itself: mark it `recovering`, resolve every
    /// pending journal entry by lookup, then mark it `interrupted` so it can
    /// be resumed or undone. Also purges expired plans.
    pub async fn reconcile(&self) {
        let db = self.deps.db.as_ref();
        match reachability_plans::Entity::delete_many()
            .filter(reachability_plans::Column::ExpiresAt.lt(Utc::now()))
            .exec(db)
            .await
        {
            Ok(done) if done.rows_affected > 0 => {
                info!(
                    purged = done.rows_affected,
                    "purged expired reachability plans"
                )
            }
            Ok(_) => {}
            Err(error) => warn!(%error, "could not purge expired reachability plans"),
        }
        let orphans = match reachability_runs::Entity::find()
            .filter(reachability_runs::Column::State.is_in(["running", "undoing", "recovering"]))
            .all(db)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "could not read reachability runs to reconcile");
                return;
            }
        };
        for row in orphans {
            if self.owned(row.environment_id) {
                continue;
            }
            self.recover(row).await;
        }
    }

    async fn recover(&self, row: reachability_runs::Model) {
        let db = self.deps.db.as_ref();
        let id = row.id;
        let was = RunState::parse(&row.state);
        let mut journal: Vec<JournalEntry> =
            serde_json::from_value(row.journal.clone()).unwrap_or_default();
        let mut active: reachability_runs::ActiveModel = row.clone().into();
        if was != RunState::Recovering {
            active.state = Set(RunState::Recovering.as_str().to_string());
            active.error = Set(Some(RECOVERING.to_string()));
            active.updated_at = Set(Utc::now());
            active.finished_at = Set(None);
            if let Err(error) = active.clone().update(db).await {
                warn!(run_id = id, %error, "could not mark a reachability run recovering");
                return;
            }
        }
        let ops = self.ops(row.project_id, row.environment_id);
        let outcome = reconcile_journal(&mut journal, &ops, id, Utc::now()).await;
        active.journal = Set(serde_json::to_value(&journal).unwrap_or(Value::Array(vec![])));
        active.updated_at = Set(Utc::now());
        if outcome.pending > 0 {
            // Stays recovering; the next pass looks again.
            let mut message = format!(
                "{RECOVERING} {} still cannot be told{}; Temps looks again every minute and never \
                 makes anything twice.",
                match outcome.pending {
                    1 => "One step".to_string(),
                    n => format!("{n} steps"),
                },
                outcome
                    .errors
                    .first()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
            if !outcome.check_by_hand.is_empty() {
                message.push_str(&format!(
                    " It has been more than an hour: {}",
                    outcome.check_by_hand.join(" ")
                ));
            }
            active.state = Set(RunState::Recovering.as_str().to_string());
            active.error = Set(Some(message));
            warn!(
                run_id = id,
                pending = outcome.pending,
                "could not recover a reachability run yet"
            );
        } else if outcome.conflicts > 0 {
            active.state = Set(RunState::Conflict.as_str().to_string());
            active.error = Set(Some(
                "Someone else changed a DNS record this run was setting. Temps leaves it alone: \
                 decide whether to keep that value or let Temps set it, then resume or undo."
                    .to_string(),
            ));
            active.finished_at = Set(Some(Utc::now()));
            info!(
                run_id = id,
                "a recovered reachability run has a conflict to decide"
            );
        } else {
            active.state = Set(RunState::Interrupted.as_str().to_string());
            active.error = Set(Some(format!(
                "This run stopped before it finished{}. What it had made is listed: resume it \
                 to carry on, or undo it to remove exactly that.",
                match outcome.resolved {
                    0 => String::new(),
                    1 => "; one unfinished step was checked".to_string(),
                    n => format!("; {n} unfinished steps were checked"),
                }
            )));
            active.finished_at = Set(Some(Utc::now()));
            info!(
                run_id = id,
                resolved = outcome.resolved,
                "recovered an interrupted reachability run"
            );
        }
        if let Err(error) = active.update(db).await {
            warn!(run_id = id, %error, "could not save a recovered reachability run");
        }
    }

    // ----- scope and facts ----------------------------------------------

    async fn scope(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Scope, ReachabilityError> {
        let project = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.deps.db.as_ref())
            .await?
            .ok_or_else(|| {
                ReachabilityError::NotFound(format!("Project {project_id} not found"))
            })?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.deps.db.as_ref())
            .await?
            .ok_or_else(|| {
                ReachabilityError::NotFound(format!(
                    "Environment {environment_id} not found in project {project_id}"
                ))
            })?;
        Ok(Scope {
            project,
            environment,
        })
    }

    async fn settings(&self) -> Result<temps_core::AppSettings, ReachabilityError> {
        self.deps
            .config
            .get_settings()
            .await
            .map_err(|e| ReachabilityError::Settings(e.to_string()))
    }

    /// Every address attached to the environment: the generated hostname,
    /// environment domains and custom domains (redirects and wildcards left
    /// out: nobody lands on those to use the app).
    async fn gather(
        &self,
        environment: &environments::Model,
        preview_domain: &str,
    ) -> Result<Gathered, ReachabilityError> {
        let mut seen = BTreeSet::new();
        let mut addresses = Vec::new();
        let expand = |domain: &str| {
            if domain.contains('.') {
                domain.trim_end_matches('.').to_ascii_lowercase()
            } else {
                PublicHostnameStrategy::Standard.environment_hostname(preview_domain, domain)
            }
        };
        let mut push = |address: String,
                        audience: Audience,
                        source,
                        id: Option<i32>,
                        private: Option<PrivateBinding>| {
            if !address.is_empty() && !address.starts_with("*.") && seen.insert(address.clone()) {
                addresses.push(AddressInput {
                    address,
                    audience,
                    source,
                    custom_domain_id: id,
                    private,
                });
            }
        };
        let customs = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::EnvironmentId.eq(environment.id))
            .order_by_asc(project_custom_domains::Column::Id)
            .all(self.deps.db.as_ref())
            .await?;
        for custom in customs {
            if custom.redirect_to.is_some() || custom.service_name.is_some() {
                continue;
            }
            let audience = Audience::parse(&custom.audience);
            push(
                custom.domain.trim_end_matches('.').to_ascii_lowercase(),
                audience,
                AddressSource::CustomDomain,
                Some(custom.id),
                (audience == Audience::Private).then(|| PrivateBinding {
                    address_id: private_address_id(true, custom.id),
                    range: custom.private_range.clone(),
                    node_id: custom.private_node_id,
                    listener: custom.private_listener.clone(),
                }),
            );
        }
        let env_domains = environment_domains::Entity::find()
            .filter(environment_domains::Column::EnvironmentId.eq(environment.id))
            .order_by_asc(environment_domains::Column::Id)
            .all(self.deps.db.as_ref())
            .await?;
        for domain in env_domains {
            let audience = Audience::parse(&domain.audience);
            push(
                expand(&domain.domain),
                audience,
                AddressSource::EnvironmentDomain,
                None,
                (audience == Audience::Private).then(|| PrivateBinding {
                    address_id: private_address_id(false, domain.id),
                    range: domain.private_range.clone(),
                    node_id: domain.private_node_id,
                    listener: domain.private_listener.clone(),
                }),
            );
        }
        if !preview_domain.trim().is_empty() && !environment.subdomain.is_empty() {
            // The generated hostname has no row; it is always public.
            push(
                expand(&environment.subdomain),
                Audience::Public,
                AddressSource::Generated,
                None,
                None,
            );
        }

        let mut running_on = Vec::new();
        if let Some(deployment_id) = environment.current_deployment_id {
            let containers = deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(self.deps.db.as_ref())
                .await?;
            let node_ids: BTreeSet<i32> = containers.iter().filter_map(|c| c.node_id).collect();
            let names: HashMap<i32, String> = nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(node_ids.iter().copied()))
                .all(self.deps.db.as_ref())
                .await?
                .into_iter()
                .map(|n| (n.id, n.name))
                .collect();
            let mut place = BTreeSet::new();
            for container in &containers {
                place.insert(match container.node_id {
                    Some(id) => names
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| format!("node {id}")),
                    None => "the control plane".to_string(),
                });
            }
            running_on = place.into_iter().collect();
        }
        Ok(Gathered {
            addresses,
            running_on,
            deployed: environment.current_deployment_id.is_some(),
        })
    }

    /// The control plane's proxy and every worker's direct ingress, with the
    /// public addresses DNS would point at.
    async fn directs(&self, settings: &temps_core::AppSettings) -> Vec<DirectIngress> {
        let mut directs = Vec::new();
        let server = self.deps.config.get_server_config();
        let takes_port_80 = server
            .address
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            == Some(80);
        // What generated records point at, else the console's own host
        // when the console is not reached through Temps Cloud.
        let mut hosts = Vec::new();
        if let Some(edge) = settings.edge_target.as_deref() {
            hosts.push(edge.to_string());
        }
        if !settings.cloud.console_access_enabled {
            if let Some(host) = settings
                .external_url
                .as_deref()
                .and_then(|url| url::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_string))
            {
                hosts.push(host);
            }
        }
        let mut ips = Vec::new();
        for host in hosts {
            for ip in resolve(&host).await.unwrap_or_default() {
                if address_scope(ip) == AddressScope::Public && !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
        if !ips.is_empty() {
            directs.push(DirectIngress {
                name: CONTROL_PLANE.to_string(),
                node_id: None,
                ips,
                takes_port_80,
                check_failed: None,
            });
        }

        let workers = nodes::Entity::find()
            .filter(nodes::Column::Role.eq("worker"))
            .filter(nodes::Column::Status.eq("active"))
            .filter(nodes::Column::PublicIngressEnabled.eq(true))
            .all(self.deps.db.as_ref())
            .await
            .unwrap_or_default();
        let ids: Vec<i32> = workers.iter().map(|w| w.id).collect();
        let rows: HashMap<i32, node_public_ingress::Model> = node_public_ingress::Entity::find()
            .filter(node_public_ingress::Column::NodeId.is_in(ids))
            .all(self.deps.db.as_ref())
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.node_id, row))
            .collect();
        for worker in workers {
            if worker.public_ingress_running != Some(true) {
                continue;
            }
            let Some(row) = rows.get(&worker.id) else {
                continue;
            };
            let interfaces: Vec<ReportedInterface> = row
                .interfaces
                .clone()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let listening_http = row
                .listening
                .as_ref()
                .and_then(|l| l.get("http"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let Some(ip) = effective_public_address(
                row.public_address.as_deref(),
                listening_http.as_deref(),
                row.listen_address.as_deref(),
                &interfaces,
            )
            .filter(|ip| address_scope(*ip) == AddressScope::Public) else {
                continue;
            };
            let check_failed = row.last_check.as_ref().and_then(|check| {
                (check.get("ok").and_then(Value::as_bool) == Some(false)).then(|| {
                    check
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("the last check failed")
                        .to_string()
                })
            });
            directs.push(DirectIngress {
                name: worker.name,
                node_id: Some(worker.id),
                ips: vec![ip],
                takes_port_80: row.http_port == 80 && listening_http.is_some(),
                check_failed,
            });
        }
        directs
    }

    /// Hostnames with a DNS record Temps manages outside the tunnels.
    async fn temps_managed(&self, hosts: &[String]) -> HashSet<String> {
        dns_managed_record_states::Entity::find()
            .filter(dns_managed_record_states::Column::Fqdn.is_in(hosts.iter().cloned()))
            .all(self.deps.db.as_ref())
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.controller != "ingress-connector")
            .map(|row| row.fqdn.trim_end_matches('.').to_ascii_lowercase())
            .collect()
    }

    async fn tunnel_states(&self, hosts: &[String]) -> HashMap<String, TunnelState> {
        match self.deps.connectors.hostname_views(hosts).await {
            Ok(views) => views
                .into_iter()
                .map(|view| {
                    (
                        view.hostname.clone(),
                        TunnelState {
                            state: view.state,
                            connector_name: view.connector_name,
                            detail: view.detail,
                            has_candidates: !view.candidates.is_empty(),
                        },
                    )
                })
                .collect(),
            Err(error) => {
                warn!(%error, "could not read tunnel publishing state");
                HashMap::new()
            }
        }
    }

    /// What the control plane knows of the workers serving `addresses`'
    /// private ones (ADR 064 D7): their listeners and inside checks.
    async fn private_facts(&self, addresses: &[AddressInput]) -> HashMap<String, PrivateFacts> {
        let node_ids: BTreeSet<i32> = addresses
            .iter()
            .filter_map(|a| a.private.as_ref()?.node_id)
            .collect();
        if node_ids.is_empty() {
            return HashMap::new();
        }
        let db = self.deps.db.as_ref();
        let nodes: HashMap<i32, nodes::Model> = nodes::Entity::find()
            .filter(nodes::Column::Id.is_in(node_ids.iter().copied()))
            .all(db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|n| (n.id, n))
            .collect();
        let rows: HashMap<i32, node_public_ingress::Model> = node_public_ingress::Entity::find()
            .filter(node_public_ingress::Column::NodeId.is_in(node_ids.iter().copied()))
            .all(db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.node_id, row))
            .collect();
        let now = Utc::now();
        addresses
            .iter()
            .filter_map(|input| {
                let binding = input.private.as_ref()?;
                let node_id = binding.node_id?;
                Some((
                    input.address.clone(),
                    private_facts_of(
                        binding,
                        node_id,
                        nodes.get(&node_id),
                        rows.get(&node_id),
                        now,
                    ),
                ))
            })
            .collect()
    }

    async fn judge(
        &self,
        addresses: &[AddressInput],
        directs: &[DirectIngress],
        deployed: bool,
        environment_name: &str,
    ) -> Vec<AddressReachability> {
        let hosts: Vec<String> = addresses.iter().map(|a| a.address.clone()).collect();
        let tunnels = self.tunnel_states(&hosts).await;
        let managed = self.temps_managed(&hosts).await;
        let privates = self.private_facts(addresses).await;
        let mut verdicts = Vec::with_capacity(addresses.len());
        for input in addresses {
            let tunnel = tunnels.get(&input.address).cloned();
            let resolution = if needs_lookup(&input.address, deployed, tunnel.as_ref()) {
                match resolve(&input.address).await {
                    Ok(ips) if ips.is_empty() => Resolution::NoRecord,
                    Ok(ips) => Resolution::Ips(ips),
                    Err(error) => Resolution::Failed(error),
                }
            } else {
                Resolution::Skipped
            };
            verdicts.push(address_reachability(
                &AddressFacts {
                    input: input.clone(),
                    tunnel,
                    resolution,
                    temps_managed_dns: managed.contains(&input.address),
                    private: privates.get(&input.address).cloned(),
                },
                directs,
                deployed,
                environment_name,
            ));
        }
        verdicts
    }

    // ----- D3 ----------------------------------------------------------

    pub async fn environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EnvironmentReachability, ReachabilityError> {
        let scope = self.scope(project_id, environment_id).await?;
        let settings = self.settings().await?;
        let gathered = self
            .gather(&scope.environment, &settings.preview_domain)
            .await?;
        let directs = self.directs(&settings).await;
        let addresses = self
            .judge(
                &gathered.addresses,
                &directs,
                gathered.deployed,
                &scope.environment.name,
            )
            .await;
        let state = overall_state(&addresses);
        let plan = if state == super::model::ReachState::Reachable {
            None
        } else {
            let facts = self
                .plan_facts(&scope, &gathered, directs, Audience::Public, None)
                .await?;
            Some(plan(&facts))
        };
        Ok(EnvironmentReachability {
            project_id,
            environment_id,
            environment_name: scope.environment.name.clone(),
            deployed: gathered.deployed,
            summary: summary(
                &addresses,
                gathered.deployed,
                &scope.environment.name,
                &gathered.running_on,
            ),
            running_on: gathered.running_on,
            state,
            addresses,
            plan,
        })
    }

    // ----- D4: plan -----------------------------------------------------

    async fn plan_facts(
        &self,
        scope: &Scope,
        gathered: &Gathered,
        directs: Vec<DirectIngress>,
        audience: Audience,
        requested: Option<String>,
    ) -> Result<PlanFacts, ReachabilityError> {
        let db = self.deps.db.as_ref();
        let mut hosts: Vec<String> = gathered
            .addresses
            .iter()
            .map(|a| a.address.clone())
            .collect();
        if let Some(requested) = requested
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            hosts.push(requested.to_string());
        }
        let tunnel_facts = self.deps.connectors.tunnel_facts(&hosts).await?;
        let served = tunnel_facts.served;
        let providers = self
            .deps
            .dns_providers
            .list_active()
            .await
            .map_err(IngressError::from)?;
        let mut zones = Vec::new();
        for provider in &providers {
            let managed = self
                .deps
                .dns_providers
                .list_managed_domains(provider.id)
                .await
                .map_err(IngressError::from)?;
            zones.extend(managed.into_iter().map(|z| ZoneFact {
                provider_id: provider.id,
                provider_name: provider.name.clone(),
                provider_type: provider.provider_type.clone(),
                domain: z.domain.trim_end_matches('.').to_ascii_lowercase(),
                verified: z.verified,
                auto_manage: z.auto_manage,
            }));
        }
        let cloudflare_accounts = providers
            .iter()
            .filter(|p| p.provider_type == "cloudflare")
            .map(|p| NamedId {
                id: p.id,
                name: p.name.clone(),
            })
            .collect();
        let workers = nodes::Entity::find()
            .filter(nodes::Column::Role.eq("worker"))
            .filter(nodes::Column::Status.eq("active"))
            .order_by_asc(nodes::Column::Id)
            .all(db)
            .await?;
        let ingress_rows = node_public_ingress::Entity::find()
            .filter(node_public_ingress::Column::NodeId.is_in(workers.iter().map(|w| w.id)))
            .all(db)
            .await?;
        let modes: HashMap<i32, Option<String>> = ingress_rows
            .iter()
            .map(|row| (row.node_id, row.mode.clone()))
            .collect();
        // ADR 064 D5: workers whose owner put them in private mode.
        let private_workers: Vec<PrivateWorkerFact> = workers
            .iter()
            .filter_map(|worker| {
                let row = ingress_rows.iter().find(|r| r.node_id == worker.id)?;
                (row.mode.as_deref() == Some("private")).then(|| private_worker_fact(worker, row))
            })
            .collect();
        let suggested_private_worker = gathered
            .running_on
            .iter()
            .find(|name| workers.iter().any(|w| &w.name == *name))
            .cloned()
            .or_else(|| workers.first().map(|w| w.name.clone()));
        let private_ranges = gathered
            .addresses
            .iter()
            .filter_map(|a| Some((a.address.clone(), a.private.as_ref()?.range.clone())))
            .collect();
        let mut tunnel_workers = Vec::new();
        let mut workers_refusing = Vec::new();
        let mut worker_modes: Vec<(i32, Option<String>)> = workers
            .iter()
            .map(|w| (w.id, modes.get(&w.id).cloned().flatten()))
            .collect();
        worker_modes.sort();
        for worker in workers {
            // Agents that report no mode predate the rule and are accepted.
            match modes.get(&worker.id).cloned().flatten().as_deref() {
                None | Some("delegated") => tunnel_workers.push(NamedId {
                    id: worker.id,
                    name: worker.name,
                }),
                Some(_) => workers_refusing.push(worker.name),
            }
        }
        let attached_here: Vec<String> = gathered
            .addresses
            .iter()
            .map(|a| a.address.clone())
            .collect();
        let taken_elsewhere = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::EnvironmentId.ne(scope.environment.id))
            .all(db)
            .await?
            .into_iter()
            .map(|d| d.domain.trim_end_matches('.').to_ascii_lowercase())
            .collect();
        let private_rule = private_rule(db, scope.project.id, scope.environment.id).await?;
        let audiences = gathered
            .addresses
            .iter()
            .map(|a| (a.address.clone(), a.audience))
            .collect();
        let mut intents = tunnel_facts.intents;
        let facts = PlanFacts {
            intents: HashMap::new(),
            choices: tunnel_facts.choices,
            private_rule,
            audiences,
            worker_modes,
            audience,
            project_slug: scope.project.slug.clone(),
            environment_slug: scope.environment.slug.clone(),
            environment_name: scope.environment.name.clone(),
            deployed: gathered.deployed,
            requested,
            served_by_workers: attached_here.iter().any(|a| served.contains(a)),
            attached_here,
            taken_elsewhere,
            tunnels: tunnel_facts.tunnels,
            zones,
            cloudflare_accounts,
            directs,
            tunnel_workers,
            workers_refusing,
            dns_records: Vec::new(),
            private_workers,
            suggested_private_worker,
            private_ranges,
        };
        let mut dns_records = Vec::new();
        // The suggested address is known only now; D9 decides it too.
        if let Ok(Some(address)) = planned_address(&facts) {
            // What a run would change is shown, and held to (D4).
            let ops = self.ops(None, None);
            for (host, record_type) in records_to_read(&facts, &address) {
                let seen = match ops.read_dns_record(&host, &record_type).await {
                    Ok(None) => PlannedRecord::Absent,
                    Ok(Some(record)) => PlannedRecord::Present { record },
                    Err(error) => {
                        warn!(%host, %error, "could not read a DNS record to plan with");
                        PlannedRecord::Unknown
                    }
                };
                dns_records.push((host, record_type, seen));
            }
            if !intents.contains_key(&address) {
                let more = self
                    .deps
                    .connectors
                    .tunnel_facts(std::slice::from_ref(&address))
                    .await?;
                intents.extend(more.intents);
            }
        }
        Ok(PlanFacts {
            intents,
            dns_records,
            ..facts
        })
    }

    async fn facts_for(
        &self,
        scope: &Scope,
        audience: Audience,
        requested: Option<String>,
    ) -> Result<(PlanFacts, temps_core::AppSettings), ReachabilityError> {
        let settings = self.settings().await?;
        let gathered = self
            .gather(&scope.environment, &settings.preview_domain)
            .await?;
        let directs = self.directs(&settings).await;
        let facts = self
            .plan_facts(scope, &gathered, directs, audience, requested)
            .await?;
        Ok((facts, settings))
    }

    /// Plan `facts` and store the plan so `apply` can hold it to them.
    async fn store_plan(
        &self,
        scope: &Scope,
        facts: &PlanFacts,
        user_id: Option<i32>,
    ) -> Result<ReachabilityPlan, ReachabilityError> {
        let mut planned = plan(facts);
        let address = planned_address(facts).ok().flatten();
        let print = fingerprint(facts, address.as_deref());
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires_at = now + PLAN_TTL;
        planned.plan_id = Some(id.clone());
        planned.fingerprint = Some(print.clone());
        planned.expires_at = Some(expires_at);
        reachability_plans::ActiveModel {
            id: Set(id),
            project_id: Set(scope.project.id),
            environment_id: Set(scope.environment.id),
            audience: Set(planned.audience.as_str().to_string()),
            address: Set(address),
            fingerprint: Set(print),
            plan: Set(serde_json::to_value(&planned).unwrap_or(Value::Null)),
            created_by: Set(user_id),
            created_at: Set(now),
            expires_at: Set(expires_at),
            used_at: Set(None),
        }
        .insert(self.deps.db.as_ref())
        .await?;
        Ok(planned)
    }

    /// The plan for the request, stored for [`PLAN_TTL`].
    pub async fn plan(
        &self,
        project_id: i32,
        environment_id: i32,
        request: PlanRequest,
        user_id: Option<i32>,
    ) -> Result<ReachabilityPlan, ReachabilityError> {
        let scope = self.scope(project_id, environment_id).await?;
        let (facts, _) = self
            .facts_for(&scope, request.audience, request.address)
            .await?;
        self.store_plan(&scope, &facts, user_id).await
    }

    async fn stale(
        &self,
        scope: &Scope,
        facts: &PlanFacts,
        user_id: Option<i32>,
        reason: &str,
    ) -> ReachabilityError {
        match self.store_plan(scope, facts, user_id).await {
            Ok(plan) => ReachabilityError::Stale {
                reason: format!("{reason} Nothing was changed; review the new plan."),
                plan: Box::new(plan),
            },
            Err(error) => error,
        }
    }

    // ----- D4: apply ----------------------------------------------------

    /// Start a run of a stored plan's option; it continues in the
    /// background. Nothing is created when the plan no longer holds.
    pub async fn apply(
        &self,
        project_id: i32,
        environment_id: i32,
        request: ApplyRequest,
        user_id: Option<i32>,
        user_email: Option<String>,
    ) -> Result<RunView, ReachabilityError> {
        let scope = self.scope(project_id, environment_id).await?;
        let db = self.deps.db.as_ref();
        let text = |key: &str| {
            request
                .answers
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let stored = reachability_plans::Entity::find_by_id(request.plan_id.clone())
            .filter(reachability_plans::Column::ProjectId.eq(project_id))
            .filter(reachability_plans::Column::EnvironmentId.eq(environment_id))
            .one(db)
            .await?;
        let answered = text(FIELD_ADDRESS);
        let requested = answered
            .clone()
            .or_else(|| stored.as_ref().and_then(|p| p.address.clone()));
        let audience = stored
            .as_ref()
            .map(|p| Audience::parse(&p.audience))
            .unwrap_or_default();
        let (facts, settings) = self.facts_for(&scope, audience, requested).await?;
        let address = match planned_address(&facts) {
            Ok(Some(address)) => address,
            Ok(None) => {
                return Err(ReachabilityError::Validation(
                    "Enter the address to make reachable.".into(),
                ))
            }
            Err(error) => return Err(ReachabilityError::Validation(error)),
        };
        let Some(stored) = stored else {
            return Err(self
                .stale(
                    &scope,
                    &facts,
                    user_id,
                    "That plan does not exist for this environment.",
                )
                .await);
        };
        if stored.used_at.is_some() {
            return Err(self
                .stale(&scope, &facts, user_id, "That plan was already applied.")
                .await);
        }
        if stored.expires_at < Utc::now() {
            return Err(self
                .stale(
                    &scope,
                    &facts,
                    user_id,
                    "That plan is more than 15 minutes old.",
                )
                .await);
        }
        if stored
            .address
            .as_deref()
            .is_some_and(|planned| planned != address)
        {
            return Err(self
                .stale(
                    &scope,
                    &facts,
                    user_id,
                    &format!(
                        "The plan was for {}, not {address}.",
                        stored.address.clone().unwrap_or_default()
                    ),
                )
                .await);
        }
        if fingerprint(&facts, Some(&address)) != stored.fingerprint {
            return Err(self
                .stale(
                    &scope,
                    &facts,
                    user_id,
                    "Something the plan depends on changed since it was made (addresses, DNS \
                     providers, tunnels, hostname choices, worker ingress, or a DNS record it \
                     would change, edited at your DNS provider).",
                )
                .await);
        }
        let planned: ReachabilityPlan =
            serde_json::from_value(stored.plan.clone()).map_err(|_| {
                ReachabilityError::Validation("That plan could not be read. Plan again.".into())
            })?;
        let Some(option) = planned.options.iter().find(|o| o.kind == request.option) else {
            return Err(ReachabilityError::Validation(match &planned.blocked {
                Some(blocked) => blocked.reason.clone(),
                None => format!(
                    "That way in is not one this plan offers for {address}. Pick one of its \
                     options."
                ),
            }));
        };
        if let (Some(confirmation), false) = (&option.confirmation, request.confirmed) {
            return Err(ReachabilityError::Validation(format!(
                "Confirm this change first: {confirmation}"
            )));
        }
        if let Some(rule) = &facts.private_rule {
            if option.way_in.starts_with("tunnel") {
                return Err(ReachabilityError::Validation(format!(
                    "{} The rule is {}.",
                    private_allow_list_message(
                        &option.way_in.replace("tunnel:", "tunnel "),
                        &rule.cidr,
                        &address
                    ),
                    rule.rule
                )));
            }
        }

        let mut input = RunInput {
            run_id: 0,
            project_id,
            environment_id,
            environment_name: scope.environment.name.clone(),
            option: option.kind,
            address: address.clone(),
            steps: option.steps.clone(),
            tunnel: None,
            new_tunnel: None,
            route_confirmed: option.confirmation.is_some() && request.confirmed,
            token: None,
            direct: None,
            dns_before: PlannedRecord::Absent,
            certificate_challenge: None,
            audience: None,
            acme_email: settings
                .letsencrypt
                .email
                .clone()
                .filter(|e| !e.trim().is_empty())
                .or(user_email),
        };
        match option.kind {
            PlanOptionKind::ExistingTunnel => {
                let route = tunnel_route(&facts, &address).ok_or_else(|| {
                    ReachabilityError::Conflict(format!("No tunnel can carry {address} any more."))
                })?;
                input.tunnel = Some(NamedId {
                    id: route.tunnel.id,
                    name: route.tunnel.name.clone(),
                });
            }
            PlanOptionKind::NewTunnel | PlanOptionKind::NewTunnelWithToken => {
                let worker = facts.tunnel_workers.first().cloned().ok_or_else(|| {
                    ReachabilityError::Conflict("No worker can run a tunnel.".into())
                })?;
                let covering_zone =
                    zone_for(&address, &facts.zones).filter(|z| z.provider_type == "cloudflare");
                let zone = covering_zone
                    .map(|z| z.domain.clone())
                    .unwrap_or_else(|| guess_zone(&address));
                let provider_id = if option.kind == PlanOptionKind::NewTunnelWithToken {
                    let token = text(FIELD_TOKEN).ok_or_else(|| {
                        ReachabilityError::Validation("Paste the Cloudflare API token.".into())
                    })?;
                    // D8: nothing starts until the token has all three
                    // permissions, for this zone only.
                    let checked = check_token(&self.http, &token, Some(&zone)).await;
                    if let Some(refusal) = checked.refusal() {
                        return Err(ReachabilityError::Validation(refusal));
                    }
                    input.token = Some(token);
                    None
                } else {
                    let chosen = request
                        .answers
                        .get(FIELD_DNS_PROVIDER)
                        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                        .and_then(|id| i32::try_from(id).ok());
                    let id = chosen
                        .or(covering_zone.map(|z| z.provider_id))
                        .or(match facts.cloudflare_accounts.as_slice() {
                            [only] => Some(only.id),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            ReachabilityError::Validation("Pick the Cloudflare account.".into())
                        })?;
                    if !facts.cloudflare_accounts.iter().any(|a| a.id == id) {
                        return Err(ReachabilityError::Validation(format!(
                            "DNS provider {id} is not an active Cloudflare account."
                        )));
                    }
                    Some(id)
                };
                let domain = tunnel_domain_for(&address, &zone);
                // Every name in use, a tunnel being removed's too: a run
                // never reuses one (ADR 064 D4).
                let taken: Vec<String> = ingress_connectors::Entity::find()
                    .all(db)
                    .await?
                    .into_iter()
                    .map(|c| c.name)
                    .collect();
                input.new_tunnel = Some(NewTunnel {
                    provider_id,
                    provider_name: self.unused_provider_name(&zone).await?,
                    name: unique_tunnel_name(&domain, &taken),
                    zone,
                    domain,
                    worker,
                });
            }
            PlanOptionKind::PrivateWorker => {
                let (worker, ip) = private_worker(&facts).ok_or_else(|| {
                    ReachabilityError::Conflict(
                        "No worker's private listener is running any more.".into(),
                    )
                })?;
                let range = temps_core::worker_ingress::private_block(ip).ok_or_else(|| {
                    ReachabilityError::Conflict(format!(
                        "{}'s listener {ip} is not a private address.",
                        worker.name
                    ))
                })?;
                input.direct = Some((worker.name.clone(), ip));
                input.dns_before = planned_record(&facts, &address, record_type_of(ip));
                // No CA can reach a private listener: DNS-01 only.
                input.certificate_challenge = Some("dns-01".to_string());
                input.audience = Some((
                    AddressAudience {
                        audience: Audience::Private,
                        network_range: Some(range.to_string()),
                        private_node_id: Some(worker.id),
                        private_listener: Some(ip.to_string()),
                    },
                    self.current_audience(&address).await?,
                ));
            }
            PlanOptionKind::DirectManagedDns | PlanOptionKind::DirectManualDns => {
                let direct = http01_direct(&facts).ok_or_else(|| {
                    ReachabilityError::Conflict("No direct ingress takes port 80 any more.".into())
                })?;
                input.direct = Some((direct.name.clone(), direct.ips[0]));
                input.dns_before = planned_record(&facts, &address, record_type_of(direct.ips[0]));
            }
        }

        // B5: an option that turns a private address public says so in a
        // SetAudience step, and puts the private one back on undo.
        if input.audience.is_none() && option.steps.iter().any(|s| s.kind == StepKind::SetAudience)
        {
            input.audience = Some((
                AddressAudience::public(),
                self.current_audience(&address).await?,
            ));
        }

        self.claim(environment_id, &scope.environment.name).await?;
        // The plan is used once: a second apply of it gets a new plan.
        let used = reachability_plans::Entity::update_many()
            .col_expr(
                reachability_plans::Column::UsedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(reachability_plans::Column::Id.eq(stored.id.clone()))
            .filter(reachability_plans::Column::UsedAt.is_null())
            .exec(db)
            .await;
        match used {
            Ok(done) if done.rows_affected == 1 => {}
            Ok(_) => {
                self.release(environment_id);
                return Err(self
                    .stale(&scope, &facts, user_id, "That plan was already applied.")
                    .await);
            }
            Err(error) => {
                self.release(environment_id);
                return Err(error.into());
            }
        }
        let record = RunRecord::new(&input.steps);
        let now = Utc::now();
        let inserted = reachability_runs::ActiveModel {
            project_id: Set(Some(project_id)),
            environment_id: Set(Some(environment_id)),
            kind: Set("apply".to_string()),
            audience: Set(audience.as_str().to_string()),
            address: Set(Some(address.clone())),
            option: Set(option.kind.as_str().to_string()),
            plan_id: Set(Some(stored.id.clone())),
            state: Set(RunState::Running.as_str().to_string()),
            steps: Set(serde_json::to_value(&record.steps).unwrap_or(Value::Array(vec![]))),
            journal: Set(Value::Array(vec![])),
            context: Set(Value::Null),
            undo: Set(None),
            error: Set(None),
            live_url: Set(None),
            created_by: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
            finished_at: Set(None),
            ..Default::default()
        }
        .insert(db)
        .await;
        let row = match inserted {
            Ok(row) => row,
            Err(error) => {
                self.release(environment_id);
                return Err(error.into());
            }
        };
        input.run_id = row.id;
        // What a resume needs; the token is never part of it.
        let mut with_context: reachability_runs::ActiveModel = row.clone().into();
        with_context.context = Set(serde_json::to_value(&input).unwrap_or(Value::Null));
        let row = match with_context.update(db).await {
            Ok(row) => row,
            Err(error) => {
                self.release(environment_id);
                return Err(error.into());
            }
        };
        info!(
            run_id = row.id,
            project_id,
            environment_id,
            option = option.kind.as_str(),
            "starting a reachability run"
        );
        let view = run_view(&row);
        self.spawn(input, None);
        Ok(view)
    }

    /// Who `host` is for now, as stored: public when it is not attached.
    async fn current_audience(&self, host: &str) -> Result<AddressAudience, ReachabilityError> {
        Ok(project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(host))
            .one(self.deps.db.as_ref())
            .await?
            .map(|row| address_audience_of(&row))
            .unwrap_or_default())
    }

    /// The audience change a stored plan's option makes, for the step-up
    /// check (ADR 064 Security): the address, who it is for now, and who
    /// it is for after. `None` when the option changes no audience.
    pub async fn audience_change(
        &self,
        project_id: i32,
        environment_id: i32,
        plan_id: &str,
        option: PlanOptionKind,
        answered_address: Option<&str>,
    ) -> Result<Option<(String, AddressAudience, Audience)>, ReachabilityError> {
        let Some(stored) = reachability_plans::Entity::find_by_id(plan_id.to_string())
            .filter(reachability_plans::Column::ProjectId.eq(project_id))
            .filter(reachability_plans::Column::EnvironmentId.eq(environment_id))
            .one(self.deps.db.as_ref())
            .await?
        else {
            return Ok(None);
        };
        let Ok(planned) = serde_json::from_value::<ReachabilityPlan>(stored.plan.clone()) else {
            return Ok(None);
        };
        let changes = planned
            .options
            .iter()
            .find(|o| o.kind == option)
            .is_some_and(|o| o.steps.iter().any(|s| s.kind == StepKind::SetAudience));
        let address = answered_address
            .map(|a| a.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .or(stored.address.clone());
        match (changes, address) {
            (true, Some(address)) => {
                let before = self.current_audience(&address).await?;
                Ok(Some((address, before, Audience::parse(&stored.audience))))
            }
            _ => Ok(None),
        }
    }

    /// A provider name nothing uses yet, so the run can find its own again.
    async fn unused_provider_name(&self, zone: &str) -> Result<String, ReachabilityError> {
        let taken: HashSet<String> = self
            .deps
            .dns_providers
            .list()
            .await
            .map_err(IngressError::from)?
            .into_iter()
            .map(|p| p.name.to_ascii_lowercase())
            .collect();
        let base = format!("Cloudflare {zone}");
        Ok(std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base} {n}")))
            .find(|name| !taken.contains(&name.to_ascii_lowercase()))
            .unwrap_or(base))
    }

    /// One run at a time per environment, and none while one is recovering.
    async fn claim(&self, environment_id: i32, name: &str) -> Result<(), ReachabilityError> {
        let recovering = reachability_runs::Entity::find()
            .filter(reachability_runs::Column::EnvironmentId.eq(environment_id))
            .filter(reachability_runs::Column::State.eq(RunState::Recovering.as_str()))
            .one(self.deps.db.as_ref())
            .await?;
        if recovering.is_some() {
            return Err(ReachabilityError::Conflict(format!(
                "{RECOVERING} Try again in a minute."
            )));
        }
        let mut active = self
            .active
            .lock()
            .map_err(|_| ReachabilityError::Conflict("Try again in a moment.".into()))?;
        if !active.insert(environment_id) {
            return Err(ReachabilityError::Conflict(format!(
                "{name} is already being set up. Wait for that run to finish."
            )));
        }
        Ok(())
    }

    /// Carry out `input` in the background: from the start, or on from
    /// `record`.
    fn spawn(&self, input: RunInput, record: Option<RunRecord>) {
        let ops = self.ops(Some(input.project_id), Some(input.environment_id));
        let sink = DbSink {
            db: self.deps.db.clone(),
            run_id: input.run_id,
        };
        let active = self.active.clone();
        let timing = self.timing;
        let environment_id = input.environment_id;
        tokio::spawn(async move {
            let record = match record {
                None => execute(&input, &ops, &sink, timing).await,
                Some(record) => resume(&input, record, &ops, &sink, timing).await,
            };
            info!(
                run_id = sink.run_id,
                state = record.state.as_str(),
                "reachability run finished"
            );
            if let Ok(mut active) = active.lock() {
                active.remove(&environment_id);
            }
        });
    }

    /// Carry on a run that is waiting or was interrupted, from its first
    /// step not done.
    pub async fn resume(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: i32,
        request: ResumeRequest,
    ) -> Result<RunView, ReachabilityError> {
        let scope = self.scope(project_id, environment_id).await?;
        let row = self.find_run(project_id, environment_id, run_id).await?;
        let state = RunState::parse(&row.state);
        match state {
            RunState::Waiting | RunState::Interrupted => {}
            RunState::Recovering => return Err(ReachabilityError::Conflict(RECOVERING.into())),
            RunState::Conflict | RunState::UndoPaused => {
                return Err(ReachabilityError::Conflict(
                    "Someone else changed a record this run works on. Decide whether to keep \
                     their value or let Temps set it first."
                        .into(),
                ))
            }
            other if other.is_active() => {
                return Err(ReachabilityError::Conflict(
                    "The run is still going.".into(),
                ))
            }
            _ => {
                return Err(ReachabilityError::Conflict(
                    "Only a waiting or interrupted run can be resumed. Plan again to start a \
                     new one."
                        .into(),
                ))
            }
        }
        let record = record_of(&row);
        if record.has_pending() {
            return Err(ReachabilityError::Conflict(RECOVERING.into()));
        }
        if record.journal.iter().any(|e| e.deleting) {
            return Err(ReachabilityError::Conflict(
                "Undo has started removing this run's tunnel, and a tunnel being removed is \
                 never reused. Undo the run again to finish, then make it reachable again."
                    .into(),
            ));
        }
        let mut input: RunInput = serde_json::from_value(row.context.clone()).map_err(|_| {
            ReachabilityError::Validation(
                "This run cannot be resumed: it was started before resuming existed. Undo it, \
                 then make it reachable again."
                    .into(),
            )
        })?;
        input.run_id = row.id;
        let token_still_needed = record.steps.iter().any(|s| {
            s.kind == StepKind::SaveDnsProvider
                && !matches!(s.status, RunStepStatus::Done | RunStepStatus::Skipped)
        });
        if token_still_needed {
            let token = request
                .answers
                .get(FIELD_TOKEN)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| {
                    ReachabilityError::Validation(
                        "Paste the Cloudflare API token again: Temps never stores it until the \
                         run saves it."
                            .into(),
                    )
                })?
                .to_string();
            if let Some(zone) = input.new_tunnel.as_ref().map(|t| t.zone.clone()) {
                if let Some(refusal) = check_token(&self.http, &token, Some(&zone)).await.refusal()
                {
                    return Err(ReachabilityError::Validation(refusal));
                }
            }
            input.token = Some(token);
        }
        self.claim(environment_id, &scope.environment.name).await?;
        let mut active: reachability_runs::ActiveModel = row.into();
        active.state = Set(RunState::Running.as_str().to_string());
        active.error = Set(None);
        active.updated_at = Set(Utc::now());
        active.finished_at = Set(None);
        let row = match active.update(self.deps.db.as_ref()).await {
            Ok(row) => row,
            Err(error) => {
                self.release(environment_id);
                return Err(error.into());
            }
        };
        info!(
            run_id,
            project_id, environment_id, "resuming a reachability run"
        );
        let view = run_view(&row);
        self.spawn(input, Some(record));
        Ok(view)
    }

    fn release(&self, environment_id: i32) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&environment_id);
        }
    }

    async fn find_run(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: i32,
    ) -> Result<reachability_runs::Model, ReachabilityError> {
        reachability_runs::Entity::find_by_id(run_id)
            .filter(reachability_runs::Column::ProjectId.eq(project_id))
            .filter(reachability_runs::Column::EnvironmentId.eq(environment_id))
            .one(self.deps.db.as_ref())
            .await?
            .ok_or_else(|| ReachabilityError::NotFound(format!("Run {run_id} not found")))
    }

    pub async fn run(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: i32,
    ) -> Result<RunView, ReachabilityError> {
        Ok(run_view(
            &self.find_run(project_id, environment_id, run_id).await?,
        ))
    }

    /// The newest runs of an environment.
    pub async fn runs(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<RunView>, ReachabilityError> {
        self.scope(project_id, environment_id).await?;
        let rows = reachability_runs::Entity::find()
            .filter(reachability_runs::Column::ProjectId.eq(project_id))
            .filter(reachability_runs::Column::EnvironmentId.eq(environment_id))
            .order_by_desc(reachability_runs::Column::CreatedAt)
            .all(self.deps.db.as_ref())
            .await?;
        Ok(rows.iter().take(20).map(run_view).collect())
    }

    /// What undo of `run_id` would still remove: what its journal says it
    /// created and was not removed yet.
    pub async fn undo_pending(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: i32,
    ) -> Result<Vec<RunResource>, ReachabilityError> {
        let row = self.find_run(project_id, environment_id, run_id).await?;
        Ok(record_of(&row).created())
    }

    /// Remove exactly what the run created, newest first. A hostname choice
    /// it changed is put back as it was.
    pub async fn undo(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: i32,
    ) -> Result<RunView, ReachabilityError> {
        let row = self.find_run(project_id, environment_id, run_id).await?;
        let state = RunState::parse(&row.state);
        let record = record_of(&row);
        if state == RunState::Recovering || record.has_pending() {
            return Err(ReachabilityError::Conflict(RECOVERING.into()));
        }
        if state.is_active() {
            return Err(ReachabilityError::Conflict(
                "The run is still going. Wait for it to finish, then undo it.".into(),
            ));
        }
        if record.created().is_empty() && record.conflicts().is_empty() {
            return Err(ReachabilityError::Conflict(
                "Nothing is left to undo: everything this run created is already gone.".into(),
            ));
        }
        {
            let mut active = self
                .active
                .lock()
                .map_err(|_| ReachabilityError::Conflict("Try again in a moment.".into()))?;
            if !active.insert(environment_id) {
                return Err(ReachabilityError::Conflict(
                    "Another run on this environment is going. Wait for it to finish.".into(),
                ));
            }
        }
        let ops = self.ops(row.project_id, row.environment_id);
        let mut journal = record.journal.clone();
        let sink = DbSink {
            db: self.deps.db.clone(),
            run_id: row.id,
        };
        let outcome = undo(&mut journal, &ops, row.id, &sink).await;
        self.release(environment_id);
        let mut all = record.undo.clone();
        let failed: Vec<String> = outcome
            .results
            .iter()
            .filter(|r| !r.ok)
            .map(|r| format!("{}: {}", r.label, r.error.clone().unwrap_or_default()))
            .collect();
        all.extend(outcome.results);
        let mut active: reachability_runs::ActiveModel = row.into();
        active.journal = Set(serde_json::to_value(&journal).unwrap_or(Value::Array(vec![])));
        active.undo = Set(Some(serde_json::to_value(&all).unwrap_or(Value::Null)));
        if let Some(stopped) = outcome.stopped {
            let (state, why) = match stopped {
                Stop::Conflict(why) => (RunState::UndoPaused, why),
                Stop::Pending(why) => (RunState::Recovering, why),
                Stop::Failed(why) => (state, why),
            };
            active.state = Set(state.as_str().to_string());
            active.error = Set(Some(if failed.is_empty() {
                format!("Undo stopped: {why}")
            } else {
                format!(
                    "Undo stopped: {why} Also not removed: {}.",
                    failed.join("; ")
                )
            }));
        } else if failed.is_empty() {
            active.state = Set(RunState::Undone.as_str().to_string());
            active.error = Set(None);
            active.live_url = Set(None);
        } else {
            active.error = Set(Some(format!(
                "Could not remove everything: {}. Undo again to retry those.",
                failed.join("; ")
            )));
        }
        active.updated_at = Set(Utc::now());
        active.finished_at = Set(Some(Utc::now()));
        let saved = active.update(self.deps.db.as_ref()).await?;
        Ok(run_view(&saved))
    }

    /// Answer a run's conflict (ADR 064 D4), journaled with its entry.
    /// Going forward the run becomes `interrupted`, to resume; while
    /// undoing, undo carries on at once.
    pub async fn decide(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: i32,
        request: DecideRequest,
    ) -> Result<RunView, ReachabilityError> {
        let row = self.find_run(project_id, environment_id, run_id).await?;
        let state = RunState::parse(&row.state);
        let undoing = match state {
            RunState::Conflict => false,
            RunState::UndoPaused => true,
            _ => {
                return Err(ReachabilityError::Conflict(
                    "This run is not waiting for a decision.".into(),
                ))
            }
        };
        let mut record = record_of(&row);
        decide(&mut record.journal, &request.key, request.decision, undoing)
            .map_err(ReachabilityError::Validation)?;
        let left = record.conflicts().len();
        let mut active: reachability_runs::ActiveModel = row.into();
        active.journal = Set(serde_json::to_value(&record.journal).unwrap_or(Value::Array(vec![])));
        active.updated_at = Set(Utc::now());
        if left == 0 {
            active.state = Set(if undoing {
                RunState::Failed.as_str().to_string()
            } else {
                RunState::Interrupted.as_str().to_string()
            });
            active.error = Set(Some(if undoing {
                "Decided; undo carries on.".to_string()
            } else {
                "Decided. Resume the run to carry on, or undo it.".to_string()
            }));
        }
        let saved = active.update(self.deps.db.as_ref()).await?;
        info!(run_id, key = %request.key, decision = ?request.decision, "reachability conflict decided");
        if undoing && left == 0 {
            return self.undo(project_id, environment_id, run_id).await;
        }
        Ok(run_view(&saved))
    }

    // ----- D8 ----------------------------------------------------------

    pub async fn token_check(&self, request: TokenCheckRequest) -> TokenCheckResult {
        check_token(&self.http, request.token.expose(), request.zone.as_deref()).await
    }

    /// Whether a direct ingress answers on port 80, so HTTP-01 can work.
    pub async fn http01_available(&self) -> Result<bool, ReachabilityError> {
        let settings = self.settings().await?;
        Ok(self
            .directs(&settings)
            .await
            .iter()
            .any(|d| d.takes_port_80 && !d.ips.is_empty()))
    }
}

/// The run's progress as stored.
fn record_of(row: &reachability_runs::Model) -> RunRecord {
    RunRecord {
        state: RunState::parse(&row.state),
        steps: serde_json::from_value::<Vec<RunStep>>(row.steps.clone()).unwrap_or_default(),
        journal: serde_json::from_value(row.journal.clone()).unwrap_or_default(),
        undo: row
            .undo
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        error: row.error.clone(),
        live_url: row.live_url.clone(),
    }
}

pub fn run_view(row: &reachability_runs::Model) -> RunView {
    let record = record_of(row);
    RunView {
        id: row.id,
        project_id: row.project_id,
        environment_id: row.environment_id,
        audience: Audience::parse(&row.audience),
        address: row.address.clone(),
        option: row.option.clone(),
        plan_id: row.plan_id.clone(),
        state: record.state,
        created: created(&record.journal),
        steps: record.steps,
        journal: record.journal,
        undo: record.undo,
        error: record.error,
        live_url: record.live_url,
        created_at: row.created_at,
        updated_at: row.updated_at,
        finished_at: row.finished_at,
    }
}

/// The IP allow list governing an environment when it lets in only private
/// ranges (ADR 064 D9): the environment's own policy, else the project's.
pub async fn private_rule(
    db: &DatabaseConnection,
    project_id: i32,
    environment_id: i32,
) -> Result<Option<PrivateAllowList>, sea_orm::DbErr> {
    let rows = project_ip_access_rules::Entity::find()
        .filter(project_ip_access_rules::Column::ProjectId.eq(project_id))
        .all(db)
        .await?;
    let own = rows
        .iter()
        .find(|r| r.environment_id == Some(environment_id));
    let (row, whose) = match own {
        Some(row) => (Some(row), "this environment's"),
        None => (
            rows.iter().find(|r| r.environment_id.is_none()),
            "the project's",
        ),
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let cidrs: Vec<String> = row.cidrs.0.iter().map(|e| e.cidr.clone()).collect();
    if row.mode != "allow" || !only_private(&cidrs) {
        return Ok(None);
    }
    Ok(Some(PrivateAllowList {
        rule: format!("{whose} IP access allow list ({})", cidrs.join(", ")),
        cidr: cidrs.first().cloned().unwrap_or_default(),
    }))
}

/// Look up `host`. `Ok(empty)` when it has no address record.
async fn resolve(host: &str) -> Result<Vec<IpAddr>, String> {
    if let Some(ip) = parse_reported_ip(host) {
        return Ok(vec![ip]);
    }
    match tokio::time::timeout(LOOKUP_TIMEOUT, tokio::net::lookup_host((host, 443))).await {
        Err(_) => Err("the lookup timed out".to_string()),
        // The resolver reports "no such name" and "no address" as errors.
        Ok(Err(_)) => Ok(Vec::new()),
        Ok(Ok(addresses)) => {
            let mut ips: Vec<IpAddr> = addresses.map(|a| a.ip()).collect();
            ips.dedup();
            Ok(ips)
        }
    }
}

#[async_trait]
impl super::run::JournalSink for DbSink {
    async fn save_journal(&self, journal: &[JournalEntry]) -> Result<(), String> {
        reachability_runs::ActiveModel {
            id: Set(self.run_id),
            journal: Set(serde_json::to_value(journal).map_err(text)?),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map(|_| ())
        .map_err(text)
    }
}

/// Saves a run's progress on its row.
struct DbSink {
    db: Arc<DatabaseConnection>,
    run_id: i32,
}

#[async_trait]
impl RunSink for DbSink {
    async fn save(&self, record: &RunRecord) -> Result<(), String> {
        let now = Utc::now();
        let journal = serde_json::to_value(&record.journal).map_err(text)?;
        let row = reachability_runs::ActiveModel {
            id: Set(self.run_id),
            state: Set(record.state.as_str().to_string()),
            steps: Set(serde_json::to_value(&record.steps).unwrap_or(Value::Null)),
            journal: Set(journal),
            undo: Set(if record.undo.is_empty() {
                None
            } else {
                serde_json::to_value(&record.undo).ok()
            }),
            error: Set(record.error.clone()),
            live_url: Set(record.live_url.clone()),
            updated_at: Set(now),
            finished_at: Set((!record.state.is_active()).then_some(now)),
            ..Default::default()
        };
        row.update(self.db.as_ref()).await.map(|_| ()).map_err(text)
    }
}

/// The run's steps against the real services.
struct LiveOps {
    deps: Arc<ReachabilityDeps>,
    http: reqwest::Client,
    /// The ADR 031 ownership scope of the DNS records a run sets.
    project_id: Option<i32>,
    environment_id: Option<i32>,
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl LiveOps {
    /// `host` as attached to the run's own environment. An address on any
    /// other environment, or a run without one, is never read or changed
    /// here: a run, or its undo, only ever acts on its own addresses.
    async fn own_address(
        &self,
        host: &str,
    ) -> Result<Option<project_custom_domains::Model>, String> {
        let Some(environment_id) = self.environment_id else {
            return Ok(None);
        };
        project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(host))
            .filter(project_custom_domains::Column::EnvironmentId.eq(environment_id))
            .one(self.deps.db.as_ref())
            .await
            .map_err(text)
    }

    fn dns_scope(&self) -> OwnershipScope {
        OwnershipScope {
            project_id: self.project_id,
            environment_id: self.environment_id,
            controller: Some(DNS_CONTROLLER),
            run_id: None,
        }
    }

    /// The record label for `host` in its managed zone.
    async fn label_of(&self, host: &str) -> Result<String, String> {
        let (_, managed) = self
            .deps
            .dns_providers
            .find_provider_for_domain(host)
            .await
            .map_err(text)?
            .ok_or_else(|| format!("No DNS provider in Temps manages {host}."))?;
        let zone = managed.domain.trim_end_matches('.').to_ascii_lowercase();
        Ok(if host == zone {
            "@".to_string()
        } else {
            host.strip_suffix(&format!(".{zone}"))
                .unwrap_or(host)
                .to_string()
        })
    }
}

#[async_trait]
impl RunOps for LiveOps {
    async fn check_token(&self, token: &str, zone: &str) -> TokenCheckResult {
        check_token(&self.http, token, Some(zone)).await
    }

    async fn find(&self, intent: &Intent) -> Result<Option<RunResource>, String> {
        let db = self.deps.db.as_ref();
        Ok(match intent {
            Intent::DnsProvider { name } => self
                .deps
                .dns_providers
                .list()
                .await
                .map_err(text)?
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| RunResource::DnsProvider {
                    id: p.id,
                    name: p.name,
                }),
            Intent::ManagedDomain {
                provider_id,
                domain,
            } => {
                self.managed_zone(*provider_id, domain)
                    .await?
                    .map(|_| RunResource::ManagedDomain {
                        provider_id: *provider_id,
                        domain: domain.clone(),
                    })
            }
            Intent::AutoManageEnabled {
                provider_id,
                domain,
            } => self
                .managed_zone(*provider_id, domain)
                .await?
                .filter(|z| z.auto_manage)
                .map(|_| RunResource::AutoManageEnabled {
                    provider_id: *provider_id,
                    domain: domain.clone(),
                }),
            Intent::Connector { name } => ingress_connectors::Entity::find()
                .filter(ingress_connectors::Column::Name.eq(name.clone()))
                .filter(ingress_connectors::Column::Status.ne(crate::service::STATUS_DELETING))
                .one(db)
                .await
                .map_err(text)?
                .map(|c| RunResource::Connector {
                    id: c.id,
                    name: c.name,
                }),
            Intent::CustomDomain {
                domain,
                environment_id,
            } => project_custom_domains::Entity::find()
                .filter(project_custom_domains::Column::Domain.eq(domain.clone()))
                .filter(project_custom_domains::Column::EnvironmentId.eq(*environment_id))
                .one(db)
                .await
                .map_err(text)?
                .map(|d| RunResource::CustomDomain {
                    id: d.id,
                    domain: domain.clone(),
                }),
            Intent::HostnameChoice {
                hostname,
                connector_id,
                previous,
            } => (self.hostname_choice(hostname).await? == Some(Some(*connector_id))).then(|| {
                RunResource::HostnameChoice {
                    hostname: hostname.clone(),
                    connector_id: *connector_id,
                    previous: *previous,
                }
            }),
            // Records are looked up by `read_dns_record`, which says who
            // wrote them (run.rs `look`).
            Intent::DnsRecord { .. } | Intent::DnsChange { .. } => {
                return Err("DNS records are looked up at their provider.".to_string())
            }
            Intent::AddressAudience {
                hostname, after, ..
            } => self
                .address_audience(hostname)
                .await?
                .filter(|current| current == after)
                .map(|_| RunResource::AddressAudience {
                    hostname: hostname.clone(),
                    before: match intent {
                        Intent::AddressAudience { before, .. } => before.clone(),
                        _ => AddressAudience::public(),
                    },
                    after: after.clone(),
                }),
            Intent::Certificate { domain } => domains::Entity::find()
                .filter(domains::Column::Domain.eq(domain.clone()))
                .one(db)
                .await
                .map_err(text)?
                .map(|_| RunResource::Certificate {
                    domain: domain.clone(),
                }),
        })
    }

    async fn save_dns_provider(&self, name: &str, _zone: &str, token: &str) -> Result<i32, String> {
        let credentials = ProviderCredentials::Cloudflare(CloudflareCredentials {
            api_token: token.to_string(),
            account_id: None,
        });
        self.deps
            .dns_providers
            .test_credentials(&DnsProviderType::Cloudflare, &credentials)
            .await
            .map_err(|e| format!("Cloudflare did not accept the token: {e}"))?;
        let provider = self
            .deps
            .dns_providers
            .create(CreateProviderRequest {
                name: name.to_string(),
                provider_type: DnsProviderType::Cloudflare,
                credentials,
                description: Some("Added by Make it reachable".to_string()),
            })
            .await
            .map_err(text)?;
        Ok(provider.id)
    }

    async fn delete_dns_provider(&self, id: i32) -> Result<(), String> {
        self.deps.dns_providers.delete(id).await.map_err(text)
    }

    async fn managed_zone(
        &self,
        provider_id: i32,
        zone: &str,
    ) -> Result<Option<ZoneState>, String> {
        Ok(self
            .deps
            .dns_providers
            .list_managed_domains(provider_id)
            .await
            .map_err(text)?
            .into_iter()
            .find(|z| z.domain.trim_end_matches('.').eq_ignore_ascii_case(zone))
            .map(|z| ZoneState {
                auto_manage: z.auto_manage,
                verified: z.verified,
            }))
    }

    async fn add_managed_zone(&self, provider_id: i32, zone: &str) -> Result<(), String> {
        self.deps
            .dns_providers
            .add_managed_domain(
                provider_id,
                AddManagedDomainRequest {
                    domain: zone.to_string(),
                    auto_manage: true,
                    proxied_by_default: false,
                    generated_hostname_mode: None,
                    sync_generated_records: false,
                },
            )
            .await
            .map(|_| ())
            .map_err(text)
    }

    async fn set_auto_manage(&self, provider_id: i32, zone: &str, on: bool) -> Result<(), String> {
        self.deps
            .dns_providers
            .update_managed_domain(
                provider_id,
                zone,
                UpdateManagedDomainRequest {
                    auto_manage: Some(on),
                    ..Default::default()
                },
            )
            .await
            .map(|_| ())
            .map_err(text)
    }

    async fn verify_zone(&self, provider_id: i32, zone: &str) -> Result<bool, String> {
        self.deps
            .dns_providers
            .verify_managed_domain(provider_id, zone)
            .await
            .map_err(text)
    }

    async fn remove_managed_zone(&self, provider_id: i32, zone: &str) -> Result<(), String> {
        self.deps
            .dns_providers
            .remove_managed_domain(provider_id, zone)
            .await
            .map_err(text)
    }

    async fn create_tunnel(
        &self,
        name: &str,
        provider_id: i32,
        domain: &str,
        node_ids: &[i32],
    ) -> Result<(i32, TunnelStatus), (Option<i32>, String)> {
        let mut values = std::collections::BTreeMap::new();
        values.insert("dns_provider_id".to_string(), Value::from(provider_id));
        values.insert("domain".to_string(), Value::from(domain));
        let request = CreateConnectorRequest {
            name: name.to_string(),
            provider: crate::cloudflare::KIND.to_string(),
            values,
            node_ids: node_ids.to_vec(),
            // Publish only what runs choose, so a tunnel on a whole zone
            // never puts other apps under it online by surprise.
            auto_publish: Some(false),
        };
        match self.deps.connectors.create(request).await {
            Ok(view) => Ok((
                view.id,
                TunnelStatus {
                    status: view.status,
                    last_error: view.last_error,
                    replicas: Vec::new(),
                },
            )),
            Err(CreateError::NotSaved(error)) => Err((None, error.to_string())),
            Err(CreateError::Saved {
                connector_id,
                error,
                ..
            }) => Err((Some(connector_id), error.to_string())),
        }
    }

    async fn tunnel_status(&self, id: i32) -> Result<TunnelStatus, String> {
        let view = self.deps.connectors.get(id).await.map_err(text)?;
        Ok(TunnelStatus {
            status: view.status,
            last_error: view.last_error,
            replicas: view
                .nodes
                .into_iter()
                .map(|n| (n.node_id, n.state, n.ready_connections, n.last_error))
                .collect(),
        })
    }

    async fn mark_tunnel_deleting(&self, id: i32, name: &str) -> Result<Option<String>, String> {
        match self.deps.connectors.mark_deleting(id, name).await {
            Ok(tunnel_id) => Ok(tunnel_id),
            Err(IngressError::NotFound(_)) => Ok(None),
            Err(error) => Err(text(error)),
        }
    }

    async fn tunnel_hostnames(&self, id: i32) -> Result<Vec<String>, String> {
        self.deps
            .connectors
            .hostnames_served(id)
            .await
            .map(|hosts| hosts.into_iter().collect())
            .map_err(text)
    }

    async fn delete_tunnel(&self, id: i32, tunnel_id: Option<&str>) -> Result<(), String> {
        self.deps
            .connectors
            .finish_deletion(id, tunnel_id)
            .await
            .map_err(text)
    }

    async fn find_custom_domain(&self, host: &str) -> Result<Option<(i32, i32)>, String> {
        Ok(project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(host))
            .one(self.deps.db.as_ref())
            .await
            .map_err(text)?
            .map(|d| (d.id, d.environment_id)))
    }

    async fn attach_domain(
        &self,
        project_id: i32,
        environment_id: i32,
        host: &str,
    ) -> Result<i32, String> {
        self.deps
            .custom_domains
            .create_custom_domain(
                project_id,
                environment_id,
                host.to_string(),
                None,
                None,
                None,
                None,
            )
            .await
            .map(|d| d.id)
            .map_err(text)
    }

    async fn detach_domain(&self, id: i32) -> Result<(), String> {
        self.deps
            .custom_domains
            .delete_custom_domain(id)
            .await
            .map_err(text)
    }

    async fn hostname_choice(&self, host: &str) -> Result<Option<Option<i32>>, String> {
        self.deps
            .connectors
            .hostname_choice(host)
            .await
            .map_err(text)
    }

    async fn route_hostname(&self, host: &str, connector_id: i32) -> Result<(), String> {
        self.deps
            .connectors
            .set_hostname_choice(
                host,
                SetHostnameChoiceRequest {
                    mode: HostnameMode::Tunnel,
                    connector_id: Some(connector_id),
                },
            )
            .await
            .map(|_| ())
            .map_err(text)
    }

    async fn restore_hostname_choice(
        &self,
        host: &str,
        previous: PreviousChoice,
    ) -> Result<(), String> {
        let request = match previous {
            PreviousChoice::Automatic => SetHostnameChoiceRequest {
                mode: HostnameMode::Automatic,
                connector_id: None,
            },
            PreviousChoice::Off => SetHostnameChoiceRequest {
                mode: HostnameMode::Off,
                connector_id: None,
            },
            PreviousChoice::Tunnel { connector_id } => SetHostnameChoiceRequest {
                mode: HostnameMode::Tunnel,
                connector_id: Some(connector_id),
            },
        };
        self.deps
            .connectors
            .set_hostname_choice(host, request)
            .await
            .map(|_| ())
            .map_err(text)
    }

    async fn hostname_state(&self, host: &str) -> Result<(String, String), String> {
        let view = self.deps.connectors.hostname(host).await.map_err(text)?;
        Ok((view.state, view.detail))
    }

    async fn read_dns_record(
        &self,
        host: &str,
        record_type: &str,
    ) -> Result<Option<DnsRecordState>, String> {
        let label = self.label_of(host).await?;
        let kind = dns_record_type(record_type);
        let snapshot = self
            .deps
            .records
            .read_record(host, &label, kind)
            .await
            .map_err(|e| format!("Could not read the {record_type} record for {host}: {e}"))?;
        Ok(snapshot.map(|s| DnsRecordState {
            provider_id: s.provider_id,
            label: label.clone(),
            record_id: s.record_id,
            record_type: s.record_type,
            content: s.content,
            modified_on: s.modified_on,
            managed: s.managed,
            run_id: s.run_id,
        }))
    }

    async fn write_dns_record(
        &self,
        host: &str,
        record_type: &str,
        content: &str,
        run_id: Option<i32>,
    ) -> Result<String, WriteError> {
        let label = self.label_of(host).await.map_err(WriteError::refused)?;
        let value = match record_type {
            "AAAA" => DnsRecordContent::AAAA {
                address: content.to_string(),
            },
            _ => DnsRecordContent::A {
                address: content.to_string(),
            },
        };
        self.deps
            .records
            .set_managed_record(
                host,
                DnsRecordRequest {
                    name: label.clone(),
                    content: value,
                    ttl: Some(300),
                    proxied: false,
                },
                Some(false),
                OwnershipScope {
                    run_id,
                    ..self.dns_scope()
                },
            )
            .await
            .map_err(|e| {
                // A refusal wrote nothing; a dropped call may have.
                let maybe_applied = matches!(
                    e,
                    temps_dns::DnsError::Request(_)
                        | temps_dns::DnsError::ConnectionFailed(_)
                        | temps_dns::DnsError::ApiError(_)
                        | temps_dns::DnsError::Database(_)
                );
                WriteError {
                    message: format!(
                        "Could not point {host} at {content}: {e}. If a record for it already \
                         exists and Temps does not manage it, remove that record first."
                    ),
                    maybe_applied,
                }
            })?;
        Ok(label)
    }

    async fn remove_dns_record(
        &self,
        host: &str,
        label: &str,
        record_type: &str,
    ) -> Result<(), String> {
        self.deps
            .records
            .remove_managed_record(host, label, dns_record_type(record_type), self.dns_scope())
            .await
            .map_err(text)
    }

    async fn certificate_exists(&self, host: &str) -> Result<bool, String> {
        let mut names = vec![host.to_string()];
        if let Some((_, parent)) = host.split_once('.') {
            names.push(format!("*.{parent}"));
        }
        Ok(domains::Entity::find()
            .filter(domains::Column::Domain.is_in(names))
            .filter(domains::Column::Status.starts_with("active"))
            .one(self.deps.db.as_ref())
            .await
            .map_err(text)?
            .is_some())
    }

    async fn start_certificate(&self, host: &str, challenge: &str) -> Result<(), String> {
        self.deps
            .domains
            .create_domain(host, challenge)
            .await
            .map(|_| ())
            .map_err(text)
    }

    async fn finish_certificate(&self, host: &str, email: &str) -> Result<(), String> {
        self.deps
            .domains
            .request_challenge(host, email)
            .await
            .map_err(|e| format!("Let's Encrypt could not start: {e}"))?;
        self.deps
            .domains
            .complete_challenge(host, email)
            .await
            .map(|_| ())
            .map_err(|e| {
                format!(
                    "Let's Encrypt could not validate {host}: {e}. Over HTTP-01 its DNS must \
                     point at this ingress with port 80 open to the internet; over DNS-01 the \
                     zone's DNS provider in Temps must be able to write TXT records."
                )
            })
    }

    async fn delete_certificate(&self, host: &str) -> Result<(), String> {
        self.deps.domains.delete_domain(host).await.map_err(text)
    }

    async fn resolve(&self, host: &str) -> Vec<IpAddr> {
        resolve(host).await.unwrap_or_default()
    }

    async fn reachability(&self, host: &str) -> AddressReachability {
        let service = ReachabilityService {
            deps: self.deps.clone(),
            http: self.http.clone(),
            active: Arc::new(Mutex::new(HashSet::new())),
            timing: Timing::default(),
        };
        let directs = match service.settings().await {
            Ok(settings) => service.directs(&settings).await,
            Err(_) => Vec::new(),
        };
        let row = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(host))
            .one(self.deps.db.as_ref())
            .await
            .ok()
            .flatten();
        let audience = row
            .as_ref()
            .map(|r| Audience::parse(&r.audience))
            .unwrap_or_default();
        let input = AddressInput {
            address: host.to_string(),
            audience,
            source: AddressSource::CustomDomain,
            custom_domain_id: row.as_ref().map(|r| r.id),
            private: row
                .as_ref()
                .filter(|_| audience == Audience::Private)
                .map(|r| PrivateBinding {
                    address_id: private_address_id(true, r.id),
                    range: r.private_range.clone(),
                    node_id: r.private_node_id,
                    listener: r.private_listener.clone(),
                }),
        };
        service
            .judge(&[input], &directs, true, "the environment")
            .await
            .into_iter()
            .next()
            .unwrap_or_else(|| AddressReachability {
                address: host.to_string(),
                audience,
                source: AddressSource::CustomDomain,
                custom_domain_id: None,
                way_in: None,
                state: super::model::ReachState::Unknown,
                reason: "Not checked.".into(),
                action: None,
                network: None,
            })
    }

    async fn address_audience(&self, host: &str) -> Result<Option<AddressAudience>, String> {
        Ok(self
            .own_address(host)
            .await?
            .map(|d| address_audience_of(&d)))
    }

    async fn set_address_audience(
        &self,
        host: &str,
        audience: &AddressAudience,
    ) -> Result<(), String> {
        let row = self
            .own_address(host)
            .await?
            .ok_or_else(|| format!("{host} is not attached to this environment any more."))?;
        let mut active: project_custom_domains::ActiveModel = row.into();
        set_address_audience_on(&mut active, audience);
        active.update(self.deps.db.as_ref()).await.map_err(text)?;
        Ok(())
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

fn dns_record_type(record_type: &str) -> DnsRecordType {
    match record_type {
        "AAAA" => DnsRecordType::AAAA,
        _ => DnsRecordType::A,
    }
}

/// Who a custom domain is for, as stored.
pub fn address_audience_of(row: &project_custom_domains::Model) -> AddressAudience {
    let audience = Audience::parse(&row.audience);
    let private = audience == Audience::Private;
    AddressAudience {
        audience,
        network_range: row.private_range.clone().filter(|_| private),
        private_node_id: row.private_node_id.filter(|_| private),
        private_listener: row.private_listener.clone().filter(|_| private),
    }
}

/// Write `audience` with what bounds it; a public one clears the private
/// fields, so nothing stale routes it later.
fn set_address_audience_on(
    active: &mut project_custom_domains::ActiveModel,
    audience: &AddressAudience,
) {
    let private = audience.audience == Audience::Private;
    active.audience = Set(audience.audience.as_str().to_string());
    active.private_range = Set(audience.network_range.clone().filter(|_| private));
    active.private_node_id = Set(audience.private_node_id.filter(|_| private));
    active.private_listener = Set(audience.private_listener.clone().filter(|_| private));
}

/// A worker in private mode as its last report describes it.
pub fn private_worker_fact(
    worker: &nodes::Model,
    row: &node_public_ingress::Model,
) -> PrivateWorkerFact {
    let listener = temps_routes::route_sync::private_listener_of(Some(row));
    let interfaces: Vec<ReportedInterface> = row
        .interfaces
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let error = row
        .listening
        .as_ref()
        .and_then(|l| l.get("error"))
        .and_then(Value::as_str)
        .filter(|e| !e.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            listener.is_none().then(|| {
                "it reports no private listener; run `temps agent ingress status` on it."
                    .to_string()
            })
        });
    PrivateWorkerFact {
        id: worker.id,
        name: worker.name.clone(),
        listener,
        tailscale: listener.is_some_and(|ip| {
            interfaces
                .iter()
                .any(|i| i.tailscale && parse_reported_ip(&i.address) == Some(ip))
        }),
        error,
    }
}

/// What the control plane knows of `node_id` serving `binding` privately:
/// whether its private listener is on the recorded address, whether that
/// is its Tailscale address, and its last inside check of the address.
pub fn private_facts_of(
    binding: &PrivateBinding,
    node_id: i32,
    node: Option<&nodes::Model>,
    row: Option<&node_public_ingress::Model>,
    now: chrono::DateTime<Utc>,
) -> PrivateFacts {
    let worker = node
        .map(|n| n.name.clone())
        .unwrap_or_else(|| format!("node {node_id}"));
    let recorded = binding
        .listener
        .as_deref()
        .and_then(|l| l.parse::<IpAddr>().ok());
    let listening_error = row
        .and_then(|r| r.listening.as_ref())
        .and_then(|l| l.get("error"))
        .and_then(Value::as_str)
        .filter(|e| !e.trim().is_empty())
        .map(str::to_string);
    let bound = match (node, row) {
        (None, _) => Err(format!("{worker} is no longer part of this cluster.")),
        (Some(n), _) if n.status != "active" => Err(format!(
            "{worker} is {}. Bring it back, or make the address reachable on another worker.",
            n.status
        )),
        (_, None) => Err(format!("{worker} has not reported its ingress yet.")),
        (_, Some(r)) if r.mode.as_deref() != Some("private") => Err(format!(
            "{worker} is not in private mode (it is `{}`). On it, as root, run              `{PRIVATE_COMMAND}`.",
            r.mode.as_deref().unwrap_or("off")
        )),
        _ if listening_error.is_some() => Err(listening_error.clone().unwrap_or_default()),
        (_, Some(r)) => match (temps_routes::route_sync::private_listener_of(Some(r)), recorded) {
            (Some(now_on), Some(recorded)) if now_on == recorded => Ok(()),
            (Some(now_on), Some(recorded)) => Err(format!(
                "its private listener is on {now_on} now, not {recorded}. Make the address                  reachable again to point it there."
            )),
            (None, _) => Err(format!("{worker}'s private listener is not running.")),
            (Some(_), None) => Err("the address records no listener.".to_string()),
        },
    };
    let tailscale = row
        .and_then(|r| r.interfaces.clone())
        .and_then(|v| serde_json::from_value::<Vec<ReportedInterface>>(v).ok())
        .unwrap_or_default()
        .iter()
        .any(|i| {
            i.tailscale && parse_reported_ip(&i.address).is_some_and(|ip| Some(ip) == recorded)
        });
    let inside = row
        .and_then(|r| r.private_checks.as_ref())
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks.iter().find(|c| {
                c.get("address_id").and_then(Value::as_str) == Some(binding.address_id.as_str())
                    && c.get("listener")
                        .and_then(Value::as_str)
                        .and_then(|l| l.parse().ok())
                        == recorded
            })
        })
        .and_then(|c| {
            let outcome = serde_json::from_value(c.get("outcome")?.clone()).ok()?;
            let received: chrono::DateTime<Utc> =
                serde_json::from_value(c.get("received_at")?.clone()).ok()?;
            Some(InsideCheck {
                outcome,
                message: c
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                minutes_ago: (now - received).num_minutes().max(0),
            })
        });
    PrivateFacts {
        worker,
        bound,
        tailscale,
        inside,
    }
}

/// ADR 064 D9: how visitors reach each address of a project's environments.
pub struct WaysIn {
    pub db: Arc<DatabaseConnection>,
    pub connectors: Arc<IngressConnectorService>,
    pub config: Arc<ConfigService>,
}

#[async_trait]
impl EnvironmentWaysIn for WaysIn {
    async fn ways_in(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<AddressWayIn>, WaysInError> {
        let failed = |reason: String| WaysInError { project_id, reason };
        let preview_domain = self
            .config
            .get_settings()
            .await
            .map_err(|e| failed(e.to_string()))?
            .preview_domain;
        let mut query = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null());
        if let Some(id) = environment_id {
            query = query.filter(environments::Column::Id.eq(id));
        }
        let envs = query
            .all(self.db.as_ref())
            .await
            .map_err(|e| failed(e.to_string()))?;
        let env_ids: Vec<i32> = envs.iter().map(|e| e.id).collect();
        let mut pairs: Vec<(i32, String)> = Vec::new();
        let expand = |domain: &str| {
            if domain.contains('.') {
                domain.trim_end_matches('.').to_ascii_lowercase()
            } else {
                PublicHostnameStrategy::Standard.environment_hostname(&preview_domain, domain)
            }
        };
        for env in &envs {
            if !preview_domain.trim().is_empty() && !env.subdomain.is_empty() {
                pairs.push((env.id, expand(&env.subdomain)));
            }
        }
        for domain in environment_domains::Entity::find()
            .filter(environment_domains::Column::EnvironmentId.is_in(env_ids.clone()))
            .all(self.db.as_ref())
            .await
            .map_err(|e| failed(e.to_string()))?
        {
            pairs.push((domain.environment_id, expand(&domain.domain)));
        }
        for domain in project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::EnvironmentId.is_in(env_ids))
            .all(self.db.as_ref())
            .await
            .map_err(|e| failed(e.to_string()))?
        {
            if domain.redirect_to.is_none() && !domain.domain.starts_with("*.") {
                pairs.push((
                    domain.environment_id,
                    domain.domain.trim_end_matches('.').to_ascii_lowercase(),
                ));
            }
        }
        pairs.retain(|(_, address)| !is_local_only(address));
        pairs.sort();
        pairs.dedup();
        let behind_proxy = self
            .behind_proxy_nodes(&envs)
            .await
            .map_err(|e| failed(e.to_string()))?;
        let hosts: Vec<String> = pairs.iter().map(|(_, a)| a.clone()).collect();
        let views: HashMap<String, (String, Option<String>)> = self
            .connectors
            .hostname_views(&hosts)
            .await
            .map_err(|e| failed(e.to_string()))?
            .into_iter()
            .map(|v| (v.hostname, (v.state, v.connector_name)))
            .collect();
        Ok(pairs
            .into_iter()
            .map(|(environment_id, address)| {
                let way_in = match views.get(&address) {
                    Some((state, Some(name))) if state == "published" || state == "pending" => {
                        WayInKind::Tunnel { name: name.clone() }
                    }
                    _ => match behind_proxy.get(&environment_id) {
                        Some(node) => WayInKind::BehindProxy { node: node.clone() },
                        None => WayInKind::Direct,
                    },
                };
                AddressWayIn {
                    environment_id,
                    address,
                    way_in,
                }
            })
            .collect())
    }
}

impl WaysIn {
    /// Per environment, a worker running it whose public ingress is
    /// `behind_proxy` (ADR 058): visitors arrive through the operator's load
    /// balancer.
    async fn behind_proxy_nodes(
        &self,
        envs: &[environments::Model],
    ) -> Result<HashMap<i32, String>, sea_orm::DbErr> {
        let db = self.db.as_ref();
        let proxied: HashMap<i32, String> = {
            let rows = node_public_ingress::Entity::find()
                .filter(node_public_ingress::Column::Mode.eq("behind_proxy"))
                .all(db)
                .await?;
            if rows.is_empty() {
                return Ok(HashMap::new());
            }
            nodes::Entity::find()
                .filter(nodes::Column::Id.is_in(rows.iter().map(|r| r.node_id)))
                .all(db)
                .await?
                .into_iter()
                .map(|n| (n.id, n.name))
                .collect()
        };
        let mut found = HashMap::new();
        for env in envs {
            let Some(deployment_id) = env.current_deployment_id else {
                continue;
            };
            let node = deployment_containers::Entity::find()
                .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                .filter(deployment_containers::Column::DeletedAt.is_null())
                .all(db)
                .await?
                .into_iter()
                .filter_map(|c| c.node_id)
                .find_map(|id| proxied.get(&id).cloned());
            if let Some(node) = node {
                found.insert(env.id, node);
            }
        }
        Ok(found)
    }
}

/// ADR 064 D9 on the tunnel side: a hostname is not moved onto a tunnel
/// while an IP allow list of only private ranges governs its environment.
#[async_trait]
impl TunnelMoveGuard for WaysIn {
    async fn check_move(&self, host: &str, tunnel: &str) -> Result<(), String> {
        let db = self.db.as_ref();
        let unreadable = |e: sea_orm::DbErr| {
            format!("Temps could not check the IP access rules of {host}'s environment: {e}")
        };
        let mut targets: Vec<(i32, i32)> = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::Domain.eq(host))
            .all(db)
            .await
            .map_err(unreadable)?
            .into_iter()
            .map(|d| (d.project_id, d.environment_id))
            .collect();
        // Environment domains and generated hostnames may be stored as the
        // label under the preview domain.
        let preview_domain = self
            .config
            .get_settings()
            .await
            .map(|s| s.preview_domain)
            .unwrap_or_default();
        let mut names = vec![host.to_string()];
        let label = host
            .strip_suffix(&format!(".{}", preview_domain.trim_start_matches("*.")))
            .filter(|label| !preview_domain.trim().is_empty() && !label.contains('.'))
            .map(str::to_string);
        names.extend(label.clone());
        let mut env_ids: Vec<i32> = environment_domains::Entity::find()
            .filter(environment_domains::Column::Domain.is_in(names))
            .all(db)
            .await
            .map_err(unreadable)?
            .into_iter()
            .map(|d| d.environment_id)
            .collect();
        if let Some(label) = label {
            env_ids.extend(
                environments::Entity::find()
                    .filter(environments::Column::Subdomain.eq(label))
                    .filter(environments::Column::DeletedAt.is_null())
                    .all(db)
                    .await
                    .map_err(unreadable)?
                    .into_iter()
                    .map(|e| e.id),
            );
        }
        if !env_ids.is_empty() {
            targets.extend(
                environments::Entity::find()
                    .filter(environments::Column::Id.is_in(env_ids))
                    .all(db)
                    .await
                    .map_err(unreadable)?
                    .into_iter()
                    .map(|e| (e.project_id, e.id)),
            );
        }
        for (project_id, environment_id) in targets {
            if let Some(rule) = private_rule(db, project_id, environment_id)
                .await
                .map_err(unreadable)?
            {
                return Err(format!(
                    "{} The rule is {}. Change or remove it first.",
                    private_allow_list_message(&format!("tunnel `{tunnel}`"), &rule.cidr, host),
                    rule.rule
                ));
            }
        }
        Ok(())
    }
}
