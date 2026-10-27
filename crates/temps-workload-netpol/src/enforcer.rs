// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node enforcement loop (ADR-055 §9, ADR-052 phase 3).
//!
//! Each pass:
//!
//! 1. reads the node's `network_policy_mode`. `legacy` stops there: no
//!    Docker call, no helper container, no ruleset. Today's behaviour.
//! 2. observes the instances on this node and identifies each one
//!    ([`identify`]) from the plane's labels, the name the control plane
//!    knows it by, or the legacy `sh.temps.environment` label;
//! 3. compiles the policies of the local workloads ([`crate::compile`]) and
//!    the anti-spoofing binding of every local bridge port
//!    ([`crate::compile_bindings`]) into one `nft -f` transaction;
//! 4. `shadow`: logs what it would enforce, applies nothing, and keeps the
//!    DNS and egress-proxy snapshots open. `enforce`: applies the
//!    transaction **only when it changed**, then publishes the DNS
//!    ([`DnsPolicy`]) and egress ([`EgressTable`]) snapshots.
//!
//! Leaving `enforce` (to `shadow` or `legacy`) removes both tables in the
//! next pass, so rolling back is one mode change.
//!
//! Control plane, not hot path: a pass is O(local instances) in memory
//! and the Docker inspect calls are O(new endpoints), cached by endpoint
//! id, so a steady node costs one container list per pass.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use temps_workload::{labels, WorkloadKind, WorkloadRef};
use tracing::{debug, info, warn};

use crate::binding::{compile_bindings, node_removal_ruleset, BindingError, PortBinding};
use crate::dns::{Alias, DnsPolicy, NamedWorkload};
use crate::ebpf::{
    cgroup_target, compile_datapath, describe_identity, CgroupDatapath, CgroupHost, CgroupTarget,
    DatapathCompileError, DatapathConfig, DatapathError, DatapathInput, DatapathOpener,
    LocalCgroupHost, CGROUP_ROOT,
};
use crate::egress::{EgressPolicy, EgressTable};
use crate::executor::{NftError, NftExecutor};
use crate::identity::IdentityMap;
use crate::nft::{compile, CompileError, TableName};
use crate::ports::{PortQuery, PortResolver};
use crate::rule::{
    Action, Cidr, Direction, L4Protocol, Origin, Peer, PortRange, Profile, Rule, WorkloadPolicy,
};

/// The legacy label deployment containers carry: the environment id.
pub const ENVIRONMENT_LABEL: &str = labels::ENVIRONMENT;

/// `nodes.network_policy_mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeMode {
    /// Nothing is compiled or applied: today's behaviour.
    #[default]
    Legacy,
    /// Computed and logged, never applied.
    Shadow,
    /// Applied.
    Enforce,
}

impl NodeMode {
    pub const ALL: [NodeMode; 3] = [NodeMode::Legacy, NodeMode::Shadow, NodeMode::Enforce];

    pub fn as_str(self) -> &'static str {
        match self {
            NodeMode::Legacy => "legacy",
            NodeMode::Shadow => "shadow",
            NodeMode::Enforce => "enforce",
        }
    }
}

impl std::fmt::Display for NodeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown network policy mode '{value}'; expected legacy, shadow or enforce")]
pub struct UnknownNodeMode {
    pub value: String,
}

impl FromStr for NodeMode {
    type Err = UnknownNodeMode;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        NodeMode::ALL
            .into_iter()
            .find(|m| m.as_str() == value)
            .ok_or_else(|| UnknownNodeMode {
                value: value.to_string(),
            })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnforcerError {
    #[error("node {node}: reading the network policy failed: {reason}")]
    Source { node: String, reason: String },
    #[error("node {node}: Docker {operation} failed: {reason}")]
    Docker {
        node: String,
        operation: String,
        reason: String,
    },
    #[error("node {node}: compiling the ruleset failed: {source}")]
    Compile {
        node: String,
        #[source]
        source: CompileError,
    },
    #[error("node {node}: compiling the address bindings failed: {source}")]
    Binding {
        node: String,
        #[source]
        source: BindingError,
    },
    #[error("node {node}: applying the ruleset failed: {source}")]
    Apply {
        node: String,
        #[source]
        source: NftError,
    },
    #[error("node {node}: compiling the eBPF datapath plan failed: {source}")]
    DatapathCompile {
        node: String,
        #[source]
        source: DatapathCompileError,
    },
    #[error("node {node}: the eBPF datapath failed: {source}")]
    Datapath {
        node: String,
        #[source]
        source: DatapathError,
    },
}

/// One network endpoint of a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub network: String,
    pub endpoint_id: String,
    pub mac: Option<String>,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
}

/// A running container as the node sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedContainer {
    pub id: String,
    /// Without Docker's leading `/`.
    pub name: String,
    pub labels: HashMap<String, String>,
    pub endpoints: Vec<Endpoint>,
}

#[async_trait]
pub trait InstanceSource: Send + Sync {
    /// The running containers on this node.
    async fn observe(&self) -> Result<Vec<ObservedContainer>, EnforcerError>;
    /// The init PID of a container, for port resolution. `None` when it
    /// is gone.
    async fn pid(&self, container_id: &str) -> Result<Option<i64>, EnforcerError>;
}

/// An address every restricted workload may reach regardless of its
/// rules: the node resolver and the egress proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPeer {
    pub cidr: Cidr,
    pub ports: Option<PortRange>,
    pub protocol: L4Protocol,
}

/// Containers every restricted workload may reach on the given ports,
/// recognised by name prefix: the per-sandbox egress proxy sidecars that
/// stay in place until the shared proxy replaces them. Their addresses are
/// taken from the same observation as everything else, so a restarted
/// sidecar is followed on the next pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemContainer {
    pub name_prefix: String,
    pub ports: Option<PortRange>,
    pub protocol: L4Protocol,
}

/// What the control plane says this node enforces. Workers receive it
/// as JSON ([`NodePolicy`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicySnapshot {
    /// Policies of workloads with something to enforce. Workloads not on
    /// this node are ignored.
    pub policies: Vec<WorkloadPolicy>,
    /// Hostname/CIDR rules for the egress proxy.
    pub egress: Vec<EgressPolicy>,
    /// Addresses of peers that run on other nodes.
    pub remote: IdentityMap,
    pub names: Vec<NamedWorkload>,
    pub aliases: Vec<Alias>,
    /// Container name → workload, for instances that predate the plane's
    /// labels (managed services, sandboxes).
    pub container_names: HashMap<String, WorkloadRef>,
    pub system_peers: Vec<SystemPeer>,
    pub system_containers: Vec<SystemContainer>,
    /// Sources every workload accepts whatever its ingress rules: the
    /// proxies and health checkers of other nodes. The node's own addresses
    /// are added by the node.
    pub system_ingress: Vec<Cidr>,
}

/// A node's mode and, outside `legacy`, its snapshot: the answer to
/// `POST /internal/nodes/{node_id}/network-policy`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePolicy {
    pub mode: NodeMode,
    #[serde(default)]
    pub snapshot: Option<PolicySnapshot>,
}

/// What a worker's enforcer tells the control plane on every pass: the
/// body of `POST /internal/nodes/{node_id}/network-policy`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodePolicyReport {
    /// The datapath it enforces with: `ebpf` or `nftables`.
    pub backend: String,
    /// The workloads it observed on its last pass outside `legacy`, with
    /// their addresses. The control plane keeps the ones inside the node's
    /// own compute subnet and hands them to every other node as remote
    /// peers. Empty in `legacy`, where the node observes nothing.
    #[serde(default)]
    pub identities: IdentityMap,
}

#[async_trait]
pub trait PolicySource: Send + Sync {
    /// The node's mode. Called every pass, before anything else.
    async fn mode(&self) -> Result<NodeMode, EnforcerError>;
    /// Only called outside `legacy`.
    async fn snapshot(&self) -> Result<PolicySnapshot, EnforcerError>;
    /// The workloads this node observed on a pass, before remote peers are
    /// merged in: what other nodes need to know about this one. Called on
    /// every pass outside `legacy`.
    fn observed(&self, _local: &IdentityMap) {}
}

/// The workload a container is an instance of.
pub fn identify(
    container: &ObservedContainer,
    container_names: &HashMap<String, WorkloadRef>,
) -> Option<WorkloadRef> {
    let label = |key: &str| container.labels.get(key).map(String::as_str);
    let environment = || {
        label(ENVIRONMENT_LABEL)
            .and_then(|id| id.parse::<i64>().ok())
            .map(|id| WorkloadRef::new(WorkloadKind::Deployment, id))
    };
    if let (Some(kind), Some(owner)) = (label(labels::KIND), label(labels::OWNER_ID)) {
        if let (Ok(kind), Ok(id)) = (kind.parse::<WorkloadKind>(), owner.parse::<i64>()) {
            // Policy subjects `deployment/N` name environment N, while the
            // plane stamps a replica's owner with its *deployment* id. The
            // environment label decides; without one the owner id must not
            // be read as an environment id (that would apply an unrelated
            // environment's rules), so the replica stays unidentified.
            if kind == WorkloadKind::Deployment {
                return environment();
            }
            return Some(WorkloadRef::new(kind, id));
        }
    }
    if let Some(workload) = container_names.get(&container.name) {
        return Some(*workload);
    }
    // `sh.temps.kind` is also used by backup helpers with values that are
    // not workload kinds; those carry no environment label.
    environment()
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassOutcome {
    /// `legacy`: nothing compiled, nothing touched.
    Legacy,
    /// Left `enforce`: both tables removed.
    Removed { mode: NodeMode },
    /// Same transaction as last pass: nothing applied.
    Unchanged { mode: NodeMode, digest: u64 },
    /// `shadow`: computed and logged.
    Shadowed(PassSummary),
    /// `enforce`: applied.
    Applied(PassSummary),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassSummary {
    pub digest: u64,
    pub local_workloads: usize,
    pub enforced_policies: usize,
    pub bindings: usize,
    pub warnings: Vec<String>,
}

/// The transaction a pass computed, before it is applied.
#[derive(Debug, Clone)]
pub struct NodePlan {
    pub ruleset: String,
    pub digest: u64,
    pub policies: Vec<WorkloadPolicy>,
    pub identities: IdentityMap,
    pub bindings: Vec<PortBinding>,
    pub warnings: Vec<String>,
    pub local: BTreeSet<WorkloadRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Installed {
    /// A fresh process: a previous one may have left tables in the kernel
    /// (rules outlive the process that applied them), so the first pass
    /// that does not enforce clears them.
    Unknown,
    Nothing,
    Shadow(u64),
    Enforced(u64),
}

pub struct NodeEnforcer {
    node: String,
    table: TableName,
    executor: Arc<dyn NftExecutor>,
    ports: Arc<dyn PortResolver>,
    instances: Arc<dyn InstanceSource>,
    policies: Arc<dyn PolicySource>,
    dns: Arc<ArcSwap<DnsPolicy>>,
    egress: Arc<ArcSwap<EgressTable>>,
    /// endpoint id → host port. Endpoint ids change when a container
    /// restarts, so a hit is always current.
    port_cache: HashMap<String, String>,
    installed: Installed,
    datapath: Option<DatapathSlot>,
    wake: Option<Arc<tokio::sync::Notify>>,
}

/// The eBPF datapath, opened on the first pass that needs it.
struct DatapathSlot {
    opener: DatapathOpener,
    pin_dir: std::path::PathBuf,
    cgroup_root: std::path::PathBuf,
    host: Arc<dyn CgroupHost>,
    open: Option<Box<dyn CgroupDatapath>>,
    /// Container id → init PID, so a steady node reads `/proc` instead of
    /// inspecting every container each pass.
    pids: HashMap<String, i64>,
    /// Mode and digest last applied.
    applied: Option<(NodeMode, u64)>,
    /// nftables tables of an earlier version were cleared (once per
    /// process), so the two backends never both enforce.
    nft_cleared: bool,
}

/// What a pass observed on this node, before any backend compiles it.
struct Observed {
    containers: Vec<ObservedContainer>,
    identities: IdentityMap,
    local: BTreeSet<WorkloadRef>,
    /// Indexes into `containers` of identified ones, with their workload.
    owned: Vec<(WorkloadRef, usize)>,
    system_peers: Vec<SystemPeer>,
}

impl NodeEnforcer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node: impl Into<String>,
        table: TableName,
        executor: Arc<dyn NftExecutor>,
        ports: Arc<dyn PortResolver>,
        instances: Arc<dyn InstanceSource>,
        policies: Arc<dyn PolicySource>,
        dns: Arc<ArcSwap<DnsPolicy>>,
        egress: Arc<ArcSwap<EgressTable>>,
    ) -> Self {
        Self {
            node: node.into(),
            table,
            executor,
            ports,
            instances,
            policies,
            dns,
            egress,
            port_cache: HashMap::new(),
            installed: Installed::Unknown,
            datapath: None,
            wake: None,
        }
    }

    /// Enforces through the eBPF datapath instead of nftables. `opener` is
    /// called by the first pass outside `legacy`, or by a `legacy` pass
    /// that finds pins under `pin_dir` (a previous process enforced).
    pub fn with_datapath(mut self, opener: DatapathOpener, config: &DatapathConfig) -> Self {
        self.datapath = Some(DatapathSlot {
            opener,
            pin_dir: config.pin_dir.clone(),
            cgroup_root: std::path::PathBuf::from(CGROUP_ROOT),
            host: Arc::new(LocalCgroupHost),
            open: None,
            pids: HashMap::new(),
            applied: None,
            nft_cleared: false,
        });
        self
    }

    /// Reads cgroups and host addresses through `host` instead of `/proc`.
    pub fn with_cgroup_host(mut self, host: Arc<dyn CgroupHost>) -> Self {
        if let Some(slot) = self.datapath.as_mut() {
            slot.host = host;
        }
        self
    }

    /// Runs a pass as soon as `wake` is notified (a container started or
    /// stopped), besides the regular interval.
    pub fn with_wake(mut self, wake: Arc<tokio::sync::Notify>) -> Self {
        self.wake = Some(wake);
        self
    }

    /// Whether passes go through the eBPF datapath.
    pub fn uses_datapath(&self) -> bool {
        self.datapath.is_some()
    }

    pub fn table(&self) -> &TableName {
        &self.table
    }

    /// One pass. See the module documentation.
    pub async fn reconcile(&mut self) -> Result<PassOutcome, EnforcerError> {
        let mode = self.policies.mode().await?;
        if self.datapath.is_some() {
            return self.reconcile_datapath(mode).await;
        }
        if mode == NodeMode::Legacy {
            match self.installed {
                Installed::Enforced(_) => {
                    self.remove(mode).await?;
                    return Ok(PassOutcome::Removed { mode });
                }
                Installed::Unknown => self.clear_leftovers(mode).await?,
                Installed::Nothing | Installed::Shadow(_) => {}
            }
            self.installed = Installed::Nothing;
            return Ok(PassOutcome::Legacy);
        }
        let snapshot = self.policies.snapshot().await?;
        let plan = self.plan(&snapshot).await?;
        let summary = PassSummary {
            digest: plan.digest,
            local_workloads: plan.local.len(),
            enforced_policies: plan.policies.iter().filter(|p| !p.is_noop()).count(),
            bindings: plan.bindings.len(),
            warnings: plan.warnings.clone(),
        };
        match mode {
            NodeMode::Legacy => Ok(PassOutcome::Legacy),
            NodeMode::Shadow => {
                match self.installed {
                    Installed::Enforced(_) => self.remove(mode).await?,
                    Installed::Unknown => self.clear_leftovers(mode).await?,
                    Installed::Nothing | Installed::Shadow(_) => {}
                }
                if self.installed == Installed::Shadow(plan.digest) {
                    return Ok(PassOutcome::Unchanged {
                        mode,
                        digest: plan.digest,
                    });
                }
                let dns = DnsPolicy::build(
                    &plan.policies,
                    &plan.identities,
                    &snapshot.names,
                    &snapshot.aliases,
                );
                info!(
                    node = %self.node,
                    table = %self.table,
                    digest = plan.digest,
                    local_workloads = summary.local_workloads,
                    enforced_policies = summary.enforced_policies,
                    bindings = summary.bindings,
                    dns_restricted_sources = dns.restricted_sources(),
                    warnings = ?summary.warnings,
                    "network policy shadow: computed, not applied"
                );
                debug!(node = %self.node, ruleset = %plan.ruleset, "network policy shadow ruleset");
                self.dns.store(Arc::new(DnsPolicy::open()));
                self.egress.store(Arc::new(EgressTable::default()));
                self.installed = Installed::Shadow(plan.digest);
                Ok(PassOutcome::Shadowed(summary))
            }
            NodeMode::Enforce => {
                // Name and proxy rules are not part of the nft digest, so
                // the snapshots are refreshed every pass; storing an Arc
                // is all it costs.
                self.publish(&plan, &snapshot);
                if self.installed == Installed::Enforced(plan.digest) {
                    return Ok(PassOutcome::Unchanged {
                        mode,
                        digest: plan.digest,
                    });
                }
                self.executor.apply(&plan.ruleset).await.map_err(|source| {
                    EnforcerError::Apply {
                        node: self.node.clone(),
                        source,
                    }
                })?;
                info!(
                    node = %self.node,
                    table = %self.table,
                    digest = plan.digest,
                    local_workloads = summary.local_workloads,
                    enforced_policies = summary.enforced_policies,
                    bindings = summary.bindings,
                    "network policy applied"
                );
                for warning in &summary.warnings {
                    warn!(node = %self.node, %warning, "network policy rule has no effect");
                }
                self.installed = Installed::Enforced(plan.digest);
                Ok(PassOutcome::Applied(summary))
            }
        }
    }

    /// One pass through the eBPF datapath.
    ///
    /// `legacy` opens nothing unless a previous process left pins, in which
    /// case the programs are switched to let everything through. Outside
    /// `legacy` the maps are made to hold the compiled plan and the mode is
    /// set, both only when something changed.
    async fn reconcile_datapath(&mut self, mode: NodeMode) -> Result<PassOutcome, EnforcerError> {
        let node = self.node.clone();
        let dp_err = |source: DatapathError| EnforcerError::Datapath {
            node: node.clone(),
            source,
        };
        if mode == NodeMode::Legacy {
            let Some(slot) = self.datapath.as_mut() else {
                return Ok(PassOutcome::Legacy);
            };
            if slot.applied.map(|(m, _)| m) == Some(NodeMode::Legacy) {
                return Ok(PassOutcome::Legacy);
            }
            if slot.open.is_none() && !slot.pin_dir.exists() {
                slot.applied = Some((NodeMode::Legacy, 0));
                return Ok(PassOutcome::Legacy);
            }
            let datapath = open_slot(slot).map_err(dp_err)?;
            datapath.set_mode(NodeMode::Legacy).map_err(dp_err)?;
            info!(node = %self.node, "network policy datapath switched to legacy: nothing is filtered");
            slot.applied = Some((NodeMode::Legacy, 0));
            self.dns.store(Arc::new(DnsPolicy::open()));
            self.egress.store(Arc::new(EgressTable::default()));
            return Ok(PassOutcome::Removed { mode });
        }

        self.clear_nft_once().await;
        let snapshot = self.policies.snapshot().await?;
        let observed = self.observe(&snapshot).await?;
        let (targets, mut warnings) = self.cgroup_targets(&observed).await?;
        let policies: Vec<WorkloadPolicy> = snapshot
            .policies
            .iter()
            .filter(|p| observed.local.contains(&p.subject))
            .cloned()
            .collect();
        let host = self
            .datapath
            .as_ref()
            .map(|s| s.host.addresses())
            .unwrap_or_default();
        let mut system_egress = observed.system_peers.clone();
        // The node itself (its resolver, its proxy) is always reachable,
        // as it is with nftables, which only filters forwarded traffic.
        system_egress.extend(host.iter().map(|addr| SystemPeer {
            cidr: Cidr::host(*addr),
            ports: None,
            protocol: L4Protocol::Any,
        }));
        let mut system_ingress: Vec<Cidr> = host.iter().map(|a| Cidr::host(*a)).collect();
        system_ingress.extend(snapshot.system_ingress.iter().copied());
        let none = BTreeSet::new();
        let plan = compile_datapath(DatapathInput {
            policies: &policies,
            identities: &observed.identities,
            local: &observed.local,
            internet: &none,
            trial: &none,
            system_egress: &system_egress,
            system_ingress: &system_ingress,
        })
        .map_err(|source| EnforcerError::DatapathCompile {
            node: self.node.clone(),
            source,
        })?;
        warnings.extend(plan.warnings.iter().cloned());
        let mut hasher = DefaultHasher::new();
        plan.digest().hash(&mut hasher);
        targets.hash(&mut hasher);
        let digest = hasher.finish();
        let summary = PassSummary {
            digest,
            local_workloads: observed.local.len(),
            enforced_policies: policies.iter().filter(|p| !p.is_noop()).count(),
            bindings: targets.len(),
            warnings,
        };
        let identities = observed.identities.clone();
        if mode == NodeMode::Enforce {
            let local_egress: Vec<EgressPolicy> = snapshot
                .egress
                .iter()
                .filter(|p| observed.local.contains(&p.subject))
                .cloned()
                .collect();
            self.dns.store(Arc::new(DnsPolicy::build(
                &policies,
                &identities,
                &snapshot.names,
                &snapshot.aliases,
            )));
            self.egress
                .store(Arc::new(EgressTable::build(&local_egress, &identities)));
        } else {
            self.dns.store(Arc::new(DnsPolicy::open()));
            self.egress.store(Arc::new(EgressTable::default()));
        }

        let Some(slot) = self.datapath.as_mut() else {
            return Ok(PassOutcome::Legacy);
        };
        if slot.applied == Some((mode, digest)) {
            log_samples(&self.node, slot);
            return Ok(PassOutcome::Unchanged { mode, digest });
        }
        let datapath = open_slot(slot).map_err(dp_err)?;
        let stats = datapath.apply(&plan, &targets).map_err(dp_err)?;
        datapath.set_mode(mode).map_err(dp_err)?;
        slot.applied = Some((mode, digest));
        info!(
            node = %self.node,
            %mode,
            digest,
            local_workloads = summary.local_workloads,
            enforced_policies = summary.enforced_policies,
            cgroups = targets.len(),
            endpoints_set = stats.endpoints_set,
            endpoints_removed = stats.endpoints_removed,
            addresses_set = stats.addresses_set,
            addresses_removed = stats.addresses_removed,
            policy_entries = stats.policy_entries,
            scopes_attached = stats.scopes_attached,
            scopes_released = stats.scopes_released,
            "network policy datapath applied"
        );
        if !stats.without_cgroup.is_empty() {
            debug!(node = %self.node, workloads = ?stats.without_cgroup, "local workloads without a running cgroup yet");
        }
        for warning in &summary.warnings {
            warn!(node = %self.node, %warning, "network policy rule has no full effect");
        }
        log_samples(&self.node, slot);
        Ok(match mode {
            NodeMode::Enforce => PassOutcome::Applied(summary),
            _ => PassOutcome::Shadowed(summary),
        })
    }

    /// Removes nftables tables an earlier version left, once per process.
    /// A host without `nft` has none, so a failure is only logged.
    async fn clear_nft_once(&mut self) {
        let Some(slot) = self.datapath.as_mut() else {
            return;
        };
        if slot.nft_cleared {
            return;
        }
        slot.nft_cleared = true;
        if let Err(e) = self.executor.clear_leftover_tables(&self.table).await {
            debug!(node = %self.node, error = %e, "no nftables network policy tables to clear");
        }
    }

    /// The cgroup of every identified container. A cgroup claimed by two
    /// different workloads identifies neither, so both stay unknown, which
    /// `enforce` refuses.
    async fn cgroup_targets(
        &mut self,
        observed: &Observed,
    ) -> Result<(Vec<CgroupTarget>, Vec<String>), EnforcerError> {
        let mut warnings = Vec::new();
        let Some((root, host)) = self
            .datapath
            .as_ref()
            .map(|s| (s.cgroup_root.clone(), s.host.clone()))
        else {
            return Ok((Vec::new(), warnings));
        };
        let live: BTreeSet<&str> = observed
            .owned
            .iter()
            .map(|(_, i)| observed.containers[*i].id.as_str())
            .collect();
        if let Some(slot) = self.datapath.as_mut() {
            slot.pids.retain(|id, _| live.contains(id.as_str()));
        }
        let mut by_id: HashMap<u64, (WorkloadRef, Option<std::path::PathBuf>)> = HashMap::new();
        let mut conflicted: BTreeSet<u64> = BTreeSet::new();
        for (workload, index) in &observed.owned {
            let container = &observed.containers[*index];
            let Some(cgroup) = self.container_cgroup(&container.id).await? else {
                continue;
            };
            let Some((dir, attach)) = cgroup_target(&root, &cgroup) else {
                continue;
            };
            let Some(id) = host.cgroup_id(&dir) else {
                continue;
            };
            match by_id.get(&id) {
                Some((other, _)) if other != workload => {
                    if conflicted.insert(id) {
                        warnings.push(format!(
                            "cgroup {} holds instances of both {other} and {workload}; neither is identified there",
                            dir.display()
                        ));
                    }
                }
                Some(_) => {}
                None => {
                    by_id.insert(id, (*workload, attach.then_some(dir)));
                }
            }
        }
        let mut targets: Vec<CgroupTarget> = by_id
            .into_iter()
            .filter(|(id, _)| !conflicted.contains(id))
            .map(|(id, (workload, attach))| CgroupTarget {
                id,
                workload,
                attach,
            })
            .collect();
        targets.sort();
        Ok((targets, warnings))
    }

    /// The unified cgroup path of a container's init process. The PID is
    /// cached; a stale one (the container restarted) is looked up again.
    async fn container_cgroup(
        &mut self,
        container_id: &str,
    ) -> Result<Option<String>, EnforcerError> {
        let Some(slot) = self.datapath.as_ref() else {
            return Ok(None);
        };
        let host = slot.host.clone();
        if let Some(pid) = slot.pids.get(container_id).copied() {
            if let Some(cgroup) = host.cgroup_of(pid) {
                return Ok(Some(cgroup));
            }
        }
        let Some(pid) = self.instances.pid(container_id).await? else {
            return Ok(None);
        };
        if let Some(slot) = self.datapath.as_mut() {
            slot.pids.insert(container_id.to_string(), pid);
        }
        Ok(host.cgroup_of(pid))
    }

    fn publish(&self, plan: &NodePlan, snapshot: &PolicySnapshot) {
        let local_egress: Vec<EgressPolicy> = snapshot
            .egress
            .iter()
            .filter(|p| plan.local.contains(&p.subject))
            .cloned()
            .collect();
        self.dns.store(Arc::new(DnsPolicy::build(
            &plan.policies,
            &plan.identities,
            &snapshot.names,
            &snapshot.aliases,
        )));
        self.egress.store(Arc::new(EgressTable::build(
            &local_egress,
            &plan.identities,
        )));
    }

    async fn remove(&mut self, mode: NodeMode) -> Result<(), EnforcerError> {
        self.executor
            .apply(&node_removal_ruleset(&self.table))
            .await
            .map_err(|source| EnforcerError::Apply {
                node: self.node.clone(),
                source,
            })?;
        self.dns.store(Arc::new(DnsPolicy::open()));
        self.egress.store(Arc::new(EgressTable::default()));
        info!(node = %self.node, table = %self.table, %mode, "network policy removed from the node");
        self.installed = Installed::Nothing;
        Ok(())
    }

    /// Clears tables a previous process may have left, once per process.
    /// On failure the state stays [`Installed::Unknown`] and the next pass
    /// retries, so a stale restriction is never silently kept.
    async fn clear_leftovers(&mut self, mode: NodeMode) -> Result<(), EnforcerError> {
        self.executor
            .clear_leftover_tables(&self.table)
            .await
            .map_err(|source| EnforcerError::Apply {
                node: self.node.clone(),
                source,
            })?;
        self.dns.store(Arc::new(DnsPolicy::open()));
        self.egress.store(Arc::new(EgressTable::default()));
        info!(node = %self.node, table = %self.table, %mode, "cleared any network policy tables left by a previous process");
        self.installed = Installed::Nothing;
        Ok(())
    }

    /// Observes this node's containers and identifies them.
    async fn observe(&self, snapshot: &PolicySnapshot) -> Result<Observed, EnforcerError> {
        let containers = self.instances.observe().await?;
        let mut identities = IdentityMap::new();
        let mut local = BTreeSet::new();
        let mut owned: Vec<(WorkloadRef, usize)> = Vec::new();
        // Every address a container on this node holds, with the workload
        // it is an instance of (`None` when unidentified).
        let mut local_holders: HashMap<IpAddr, Option<WorkloadRef>> = HashMap::new();
        for (index, container) in containers.iter().enumerate() {
            let workload = identify(container, &snapshot.container_names);
            for addr in container_addresses(container) {
                local_holders
                    .entry(addr)
                    .and_modify(|holder| {
                        if *holder != workload {
                            // Two different holders: owned by neither.
                            *holder = None;
                        }
                    })
                    .or_insert(workload);
            }
            let Some(workload) = workload else {
                continue;
            };
            local.insert(workload);
            for addr in container_addresses(container) {
                identities.insert(workload, addr);
            }
            owned.push((workload, index));
        }
        self.policies.observed(&identities);
        merge_remote(
            &self.node,
            &mut identities,
            &snapshot.remote,
            &local_holders,
        );

        let mut system_peers = snapshot.system_peers.clone();
        for container in &containers {
            let Some(system) = snapshot
                .system_containers
                .iter()
                .find(|s| container.name.starts_with(&s.name_prefix))
            else {
                continue;
            };
            for addr in container_addresses(container) {
                system_peers.push(SystemPeer {
                    cidr: Cidr::host(addr),
                    ports: system.ports,
                    protocol: system.protocol,
                });
            }
        }
        Ok(Observed {
            containers,
            identities,
            local,
            owned,
            system_peers,
        })
    }

    /// Computes the transaction for the current instances and policies.
    pub async fn plan(&mut self, snapshot: &PolicySnapshot) -> Result<NodePlan, EnforcerError> {
        let Observed {
            containers,
            identities,
            local,
            owned,
            system_peers,
        } = self.observe(snapshot).await?;
        let owned: Vec<(WorkloadRef, &ObservedContainer)> = owned
            .into_iter()
            .map(|(workload, index)| (workload, &containers[index]))
            .collect();

        let mut policies: Vec<WorkloadPolicy> = snapshot
            .policies
            .iter()
            .filter(|p| local.contains(&p.subject))
            .cloned()
            .collect();
        for policy in &mut policies {
            if policy.egress == Profile::Restricted {
                for (index, peer) in system_peers.iter().enumerate() {
                    policy.rules.push(Rule {
                        id: -(index as i64) - 1,
                        direction: Direction::Egress,
                        peer: Peer::Cidr(peer.cidr),
                        ports: peer.ports,
                        protocol: peer.protocol,
                        action: Action::Allow,
                        origin: Origin::System,
                    });
                }
            }
        }

        let compiled = compile(&self.table, &policies, &identities).map_err(|source| {
            EnforcerError::Compile {
                node: self.node.clone(),
                source,
            }
        })?;
        let bindings = self.bindings(&owned).await?;
        let bridge =
            compile_bindings(&self.table, &bindings).map_err(|source| EnforcerError::Binding {
                node: self.node.clone(),
                source,
            })?;
        let mut ruleset = compiled.ruleset;
        ruleset.push_str(&bridge);
        let mut hasher = DefaultHasher::new();
        ruleset.hash(&mut hasher);
        Ok(NodePlan {
            digest: hasher.finish(),
            ruleset,
            policies,
            identities,
            bindings,
            warnings: compiled.warnings,
            local,
        })
    }

    async fn bindings(
        &mut self,
        owned: &[(WorkloadRef, &ObservedContainer)],
    ) -> Result<Vec<PortBinding>, EnforcerError> {
        let live: BTreeSet<&str> = owned
            .iter()
            .flat_map(|(_, c)| c.endpoints.iter().map(|e| e.endpoint_id.as_str()))
            .collect();
        self.port_cache.retain(|id, _| live.contains(id.as_str()));

        let mut queries: Vec<(PortQuery, String)> = Vec::new();
        let mut pids: HashMap<&str, Option<i64>> = HashMap::new();
        for (_, container) in owned {
            for endpoint in &container.endpoints {
                let Some(mac) = endpoint.mac.as_deref().filter(|m| !m.is_empty()) else {
                    continue;
                };
                if endpoint.endpoint_id.is_empty()
                    || self.port_cache.contains_key(&endpoint.endpoint_id)
                {
                    continue;
                }
                let pid = match pids.get(container.id.as_str()) {
                    Some(pid) => *pid,
                    None => {
                        let pid = self.instances.pid(&container.id).await?;
                        pids.insert(&container.id, pid);
                        pid
                    }
                };
                if let Some(pid) = pid.filter(|p| *p > 0) {
                    queries.push((
                        PortQuery {
                            pid,
                            mac: mac.to_ascii_lowercase(),
                        },
                        endpoint.endpoint_id.clone(),
                    ));
                }
            }
        }
        if !queries.is_empty() {
            let plain: Vec<PortQuery> = queries.iter().map(|(q, _)| q.clone()).collect();
            let found =
                self.ports
                    .resolve(&plain)
                    .await
                    .map_err(|source| EnforcerError::Apply {
                        node: self.node.clone(),
                        source,
                    })?;
            for (query, endpoint_id) in queries {
                if let Some(port) = found.get(&query) {
                    self.port_cache.insert(endpoint_id, port.clone());
                }
            }
        }

        let mut bindings = Vec::new();
        for (workload, container) in owned {
            for endpoint in &container.endpoints {
                let Some(port) = self.port_cache.get(&endpoint.endpoint_id) else {
                    continue;
                };
                let mut binding = PortBinding::new(*workload, port.clone());
                if let Some(v4) = endpoint.ipv4 {
                    binding = binding.with(IpAddr::V4(v4));
                }
                if let Some(v6) = endpoint.ipv6 {
                    binding = binding.with(IpAddr::V6(v6));
                }
                bindings.push(binding);
            }
        }
        Ok(bindings)
    }

    /// Runs passes until the task is dropped. A failed pass is logged and
    /// retried on the next tick; it never takes the node down.
    pub async fn run(mut self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let wake = self.wake.clone();
        loop {
            match &wake {
                Some(wake) => {
                    tokio::select! {
                        _ = ticker.tick() => {}
                        _ = wake.notified() => {
                            // A burst of container events (a rollout) is
                            // one pass.
                            tokio::time::sleep(WAKE_DEBOUNCE).await;
                            ticker.reset();
                        }
                    }
                }
                None => {
                    ticker.tick().await;
                }
            }
            match self.reconcile().await {
                Ok(PassOutcome::Legacy) | Ok(PassOutcome::Unchanged { .. }) => {}
                Ok(outcome) => debug!(node = %self.node, ?outcome, "network policy pass"),
                Err(e) => warn!(node = %self.node, error = %e, "network policy pass failed"),
            }
        }
    }
}

/// How long a pass waits after a wake-up, so a burst of events is one pass.
const WAKE_DEBOUNCE: Duration = Duration::from_millis(150);

/// Most blocked-flow samples logged per pass.
const SAMPLES_PER_PASS: usize = 32;

fn open_slot(slot: &mut DatapathSlot) -> Result<&mut Box<dyn CgroupDatapath>, DatapathError> {
    if slot.open.is_none() {
        slot.open = Some((slot.opener)()?);
    }
    slot.open.as_mut().ok_or(DatapathError::NotBuiltIn)
}

/// Logs the flows the programs refused (or would have) since the last pass.
fn log_samples(node: &str, slot: &mut DatapathSlot) {
    let Some(datapath) = slot.open.as_mut() else {
        return;
    };
    for sample in datapath.drain_samples(SAMPLES_PER_PASS) {
        let addr: IpAddr = if sample.family == temps_netpol_ebpf_common::AF_INET {
            IpAddr::V4(Ipv4Addr::new(
                sample.addr[0],
                sample.addr[1],
                sample.addr[2],
                sample.addr[3],
            ))
        } else {
            IpAddr::V6(Ipv6Addr::from(sample.addr))
        };
        let direction = if sample.dir == temps_netpol_ebpf_common::DIR_EGRESS {
            "egress"
        } else {
            "ingress"
        };
        info!(
            node = %node,
            subject = %describe_identity(sample.subject),
            peer = %describe_identity(sample.peer),
            %addr,
            port = sample.port,
            protocol = sample.proto,
            direction,
            rule = sample.rule,
            would_block = sample.would_block == 1,
            "network policy refused a flow"
        );
    }
}

/// The IPv4 and IPv6 addresses of every endpoint of a container.
fn container_addresses(container: &ObservedContainer) -> impl Iterator<Item = IpAddr> + '_ {
    container.endpoints.iter().flat_map(|endpoint| {
        endpoint
            .ipv4
            .map(IpAddr::V4)
            .into_iter()
            .chain(endpoint.ipv6.map(IpAddr::V6))
    })
}

/// Adds the worker-hosted addresses the control plane recorded to the
/// identities observed on this node.
///
/// A workload may run both here and elsewhere (a database cluster with a
/// local monitor and data members on workers), so every remote address is
/// kept whether or not the workload also has a local instance: a peer set
/// must hold all of a workload's instances, or an allow misses the remote
/// members and a deny lets them through.
///
/// What this node observes is authoritative for the addresses it holds,
/// though. A remote record naming an address that a local container of a
/// *different* workload (or an unidentified one) holds is stale or
/// colliding; merging it would hand that local container the remote
/// workload's identity, so it is skipped. A record repeating the address of
/// a local instance of the same workload is harmless (sets deduplicate).
fn merge_remote(
    node: &str,
    identities: &mut IdentityMap,
    remote: &IdentityMap,
    local_holders: &HashMap<IpAddr, Option<WorkloadRef>>,
) {
    for workload in remote.workloads() {
        for addr in remote.addresses(workload) {
            if let Some(holder) = local_holders.get(&addr) {
                if holder.as_ref() != Some(workload) {
                    debug!(
                        node = %node,
                        remote_workload = %workload,
                        local_holder = ?holder,
                        %addr,
                        "remote address is held by another container on this node; skipped"
                    );
                    continue;
                }
            }
            identities.insert(*workload, addr);
        }
    }
}

/// Observes the containers of the local Docker daemon.
pub struct DockerInstanceSource {
    node: String,
    docker: Docker,
}

impl DockerInstanceSource {
    pub fn new(node: impl Into<String>, docker: Docker) -> Self {
        Self {
            node: node.into(),
            docker,
        }
    }
}

#[async_trait]
impl InstanceSource for DockerInstanceSource {
    async fn observe(&self) -> Result<Vec<ObservedContainer>, EnforcerError> {
        let summaries = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(|e| EnforcerError::Docker {
                node: self.node.clone(),
                operation: "list containers".to_string(),
                reason: e.to_string(),
            })?;
        Ok(summaries
            .into_iter()
            .filter_map(|summary| {
                let id = summary.id?;
                let name = summary
                    .names
                    .and_then(|names| names.into_iter().next())
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_default();
                let endpoints = summary
                    .network_settings
                    .and_then(|s| s.networks)
                    .map(|networks| {
                        let mut endpoints: Vec<Endpoint> = networks
                            .into_iter()
                            .map(|(network, e)| Endpoint {
                                network,
                                endpoint_id: e.endpoint_id.unwrap_or_default(),
                                mac: e.mac_address.filter(|m| !m.is_empty()),
                                ipv4: e.ip_address.and_then(|a| a.parse().ok()),
                                ipv6: e.global_ipv6_address.and_then(|a| a.parse().ok()),
                            })
                            .collect();
                        endpoints.sort_by(|a, b| a.network.cmp(&b.network));
                        endpoints
                    })
                    .unwrap_or_default();
                Some(ObservedContainer {
                    id,
                    name,
                    labels: summary.labels.unwrap_or_default(),
                    endpoints,
                })
            })
            .collect())
    }

    async fn pid(&self, container_id: &str) -> Result<Option<i64>, EnforcerError> {
        match self.docker.inspect_container(container_id, None).await {
            Ok(inspect) => Ok(inspect.state.and_then(|s| s.pid).filter(|p| *p > 0)),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(EnforcerError::Docker {
                node: self.node.clone(),
                operation: format!("inspect container {container_id}"),
                reason: e.to_string(),
            }),
        }
    }
}

/// Picks the node's datapath: eBPF when this build embeds the programs, the
/// Docker daemon is on this host and the host meets the requirements;
/// nftables otherwise. The choice and the reason are logged once.
pub fn with_best_datapath(enforcer: NodeEnforcer, daemon_is_local: bool) -> NodeEnforcer {
    if !daemon_is_local {
        info!(
            node = %enforcer.node,
            "network policy backend: nftables (the Docker daemon is not on this host, so its containers' cgroups are not visible)"
        );
        return enforcer;
    }
    let config = DatapathConfig::default();
    match crate::ebpf::available_datapath(config.clone()) {
        Ok(opener) => {
            info!(
                node = %enforcer.node,
                pin_dir = %config.pin_dir.display(),
                "network policy backend: eBPF (loaded on the first pass outside legacy)"
            );
            enforcer.with_datapath(opener, &config)
        }
        Err(reason) => {
            info!(node = %enforcer.node, %reason, "network policy backend: nftables");
            enforcer
        }
    }
}

/// Container events that change what a node enforces.
const WAKE_EVENTS: [&str; 4] = ["start", "die", "connect", "disconnect"];

/// Notifies the returned handle whenever a container starts, stops or
/// changes networks on the local daemon, so the enforcer identifies a new
/// container within a pass instead of an interval. The watch reconnects
/// with a growing delay when the stream ends.
pub fn spawn_container_wake(node: String, docker: Docker) -> Arc<tokio::sync::Notify> {
    use futures::StreamExt;
    let wake = Arc::new(tokio::sync::Notify::new());
    let notify = wake.clone();
    tokio::spawn(async move {
        let mut delay = Duration::from_secs(1);
        loop {
            let mut filters: HashMap<String, Vec<String>> = HashMap::new();
            filters.insert("type".to_string(), vec!["container".to_string()]);
            filters.insert(
                "event".to_string(),
                WAKE_EVENTS.iter().map(|e| e.to_string()).collect(),
            );
            let mut stream = docker.events(Some(bollard::query_parameters::EventsOptions {
                filters: Some(filters),
                ..Default::default()
            }));
            while let Some(event) = stream.next().await {
                match event {
                    Ok(_) => {
                        delay = Duration::from_secs(1);
                        notify.notify_one();
                    }
                    Err(e) => {
                        debug!(node = %node, error = %e, "container event stream failed; reconnecting");
                        break;
                    }
                }
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    });
    wake
}

/// A fixed snapshot, for tests and for nodes whose policy is pushed
/// rather than read.
pub struct StaticPolicySource {
    mode: ArcSwap<NodeMode>,
    snapshot: ArcSwap<PolicySnapshot>,
    observed: ArcSwap<IdentityMap>,
}

impl StaticPolicySource {
    pub fn new(mode: NodeMode, snapshot: PolicySnapshot) -> Self {
        Self {
            mode: ArcSwap::from_pointee(mode),
            snapshot: ArcSwap::from_pointee(snapshot),
            observed: ArcSwap::from_pointee(IdentityMap::new()),
        }
    }

    /// The local workloads the last pass observed.
    pub fn last_observed(&self) -> IdentityMap {
        (**self.observed.load()).clone()
    }

    pub fn set_mode(&self, mode: NodeMode) {
        self.mode.store(Arc::new(mode));
    }

    pub fn set_snapshot(&self, snapshot: PolicySnapshot) {
        self.snapshot.store(Arc::new(snapshot));
    }
}

#[async_trait]
impl PolicySource for StaticPolicySource {
    async fn mode(&self) -> Result<NodeMode, EnforcerError> {
        Ok(**self.mode.load())
    }

    async fn snapshot(&self) -> Result<PolicySnapshot, EnforcerError> {
        Ok((**self.snapshot.load()).clone())
    }

    fn observed(&self, local: &IdentityMap) {
        self.observed.store(Arc::new(local.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct RecordingExecutor {
        applied: Mutex<Vec<String>>,
        /// Makes the next apply fail once, as a missing helper would.
        fail_next: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl NftExecutor for RecordingExecutor {
        async fn apply(&self, ruleset: &str) -> Result<(), NftError> {
            if self.fail_next.swap(false, Ordering::Relaxed) {
                return Err(NftError::Timeout {
                    location: self.location(),
                    timeout_secs: 1,
                });
            }
            self.applied.lock().await.push(ruleset.to_string());
            Ok(())
        }
        async fn remove(&self, _table: &TableName) -> Result<(), NftError> {
            Ok(())
        }
        async fn list(&self, _table: &TableName) -> Result<Option<String>, NftError> {
            Ok(None)
        }
        fn location(&self) -> String {
            "a test recorder".to_string()
        }
    }

    struct FixedInstances {
        containers: std::sync::Mutex<Vec<ObservedContainer>>,
        observed: AtomicUsize,
    }

    #[async_trait]
    impl InstanceSource for FixedInstances {
        async fn observe(&self) -> Result<Vec<ObservedContainer>, EnforcerError> {
            self.observed.fetch_add(1, Ordering::Relaxed);
            Ok(self
                .containers
                .lock()
                .map(|c| c.clone())
                .unwrap_or_default())
        }
        async fn pid(&self, container_id: &str) -> Result<Option<i64>, EnforcerError> {
            Ok(container_id.trim_start_matches('c').parse().ok())
        }
    }

    struct CountingPorts {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl PortResolver for CountingPorts {
        async fn resolve(
            &self,
            queries: &[PortQuery],
        ) -> Result<HashMap<PortQuery, String>, NftError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(queries
                .iter()
                .map(|q| (q.clone(), format!("veth{}", q.pid)))
                .collect())
        }
    }

    fn container(id: u32, labels: &[(&str, &str)], ip: &str) -> ObservedContainer {
        ObservedContainer {
            id: format!("c{id}"),
            name: format!("app-{id}"),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            endpoints: vec![Endpoint {
                network: "temps".into(),
                endpoint_id: format!("ep{id}"),
                mac: Some(format!("02:42:0a:00:00:{id:02x}")),
                ipv4: Some(ip.parse().unwrap()),
                ipv6: None,
            }],
        }
    }

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }

    fn service(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Service, id)
    }

    struct Harness {
        enforcer: NodeEnforcer,
        executor: Arc<RecordingExecutor>,
        policies: Arc<StaticPolicySource>,
        instances: Arc<FixedInstances>,
        ports: Arc<CountingPorts>,
        dns: Arc<ArcSwap<DnsPolicy>>,
        egress: Arc<ArcSwap<EgressTable>>,
    }

    fn harness_snapshot() -> PolicySnapshot {
        PolicySnapshot {
            policies: vec![WorkloadPolicy {
                subject: sandbox(1),
                egress: Profile::Restricted,
                ingress: Profile::Open,
                rules: vec![Rule {
                    id: 1,
                    direction: Direction::Egress,
                    peer: Peer::Workload(service(5)),
                    ports: Some(PortRange::single(5432)),
                    protocol: L4Protocol::Tcp,
                    action: Action::Allow,
                    origin: Origin::Link,
                }],
            }],
            container_names: HashMap::from([("app-2".to_string(), service(5))]),
            system_peers: vec![SystemPeer {
                cidr: "10.237.1.53/32".parse().unwrap(),
                ports: Some(PortRange::single(53)),
                protocol: L4Protocol::Any,
            }],
            names: vec![NamedWorkload {
                fqdn: "db.shop.temps.local".into(),
                owner: service(5),
            }],
            ..Default::default()
        }
    }

    fn harness(mode: NodeMode) -> Harness {
        let executor = Arc::new(RecordingExecutor::default());
        let instances = Arc::new(FixedInstances {
            containers: std::sync::Mutex::new(vec![
                container(
                    1,
                    &[(labels::KIND, "sandbox"), (labels::OWNER_ID, "1")],
                    "10.237.1.2",
                ),
                container(2, &[], "10.237.1.3"),
                container(3, &[(ENVIRONMENT_LABEL, "7")], "10.237.1.4"),
            ]),
            observed: AtomicUsize::new(0),
        });
        let snapshot = harness_snapshot();
        let policies = Arc::new(StaticPolicySource::new(mode, snapshot));
        let ports = Arc::new(CountingPorts {
            calls: AtomicUsize::new(0),
        });
        let dns = Arc::new(ArcSwap::from_pointee(DnsPolicy::open()));
        let egress = Arc::new(ArcSwap::from_pointee(EgressTable::default()));
        let enforcer = NodeEnforcer::new(
            "node-a",
            TableName::new("temps_netpol_t").unwrap(),
            executor.clone(),
            ports.clone(),
            instances.clone(),
            policies.clone(),
            dns.clone(),
            egress.clone(),
        );
        Harness {
            enforcer,
            executor,
            policies,
            instances,
            ports,
            dns,
            egress,
        }
    }

    #[test]
    fn modes_parse_and_print() {
        for mode in NodeMode::ALL {
            assert_eq!(mode.as_str().parse::<NodeMode>(), Ok(mode));
        }
        assert_eq!(NodeMode::default(), NodeMode::Legacy);
        assert!("off".parse::<NodeMode>().is_err());
    }

    #[test]
    fn containers_are_identified_by_plane_labels_then_names_then_environment() {
        let names = HashMap::from([("app-2".to_string(), service(5))]);
        let plane = container(
            1,
            &[(labels::KIND, "job"), (labels::OWNER_ID, "4")],
            "10.0.0.1",
        );
        assert_eq!(
            identify(&plane, &names),
            Some(WorkloadRef::new(WorkloadKind::Job, 4))
        );
        assert_eq!(
            identify(&container(2, &[], "10.0.0.2"), &names),
            Some(service(5))
        );
        assert_eq!(
            identify(
                &container(3, &[(ENVIRONMENT_LABEL, "7")], "10.0.0.3"),
                &names
            ),
            Some(WorkloadRef::new(WorkloadKind::Deployment, 7))
        );
        let helper = container(9, &[(labels::KIND, "backup")], "10.0.0.9");
        assert_eq!(
            identify(&helper, &names),
            None,
            "backup helpers are not workloads"
        );
    }

    #[test]
    fn plane_deployment_replicas_are_identified_by_their_environment() {
        let names = HashMap::new();
        // Revision 42 of environment 7, as the plane labels it: the policy
        // subject is deployment/7, never deployment/42.
        let replica = container(
            1,
            &[
                (labels::KIND, "deployment"),
                (labels::OWNER_ID, "42"),
                (ENVIRONMENT_LABEL, "7"),
            ],
            "10.0.0.1",
        );
        assert_eq!(
            identify(&replica, &names),
            Some(WorkloadRef::new(WorkloadKind::Deployment, 7))
        );
        // Without an environment label the deployment id is not an
        // environment id: no identity rather than someone else's rules.
        let unlabelled = container(
            2,
            &[(labels::KIND, "deployment"), (labels::OWNER_ID, "42")],
            "10.0.0.2",
        );
        assert_eq!(identify(&unlabelled, &names), None);
        // A legacy deployment container carries only the environment label.
        let legacy = container(3, &[(ENVIRONMENT_LABEL, "7")], "10.0.0.3");
        assert_eq!(
            identify(&legacy, &names),
            Some(WorkloadRef::new(WorkloadKind::Deployment, 7))
        );
    }

    #[tokio::test]
    async fn legacy_touches_nothing_after_clearing_leftovers() {
        let mut h = harness(NodeMode::Legacy);
        // The first pass of a process clears what a previous one may have
        // left (see a_restarted_legacy_node_clears_tables_a_previous_process_left).
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        h.executor.applied.lock().await.clear();
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        assert!(h.executor.applied.lock().await.is_empty());
        assert_eq!(
            h.instances.observed.load(Ordering::Relaxed),
            0,
            "no Docker call"
        );
        assert_eq!(h.ports.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn shadow_computes_without_applying_or_restricting() {
        let mut h = harness(NodeMode::Shadow);
        let outcome = h.enforcer.reconcile().await.unwrap();
        let PassOutcome::Shadowed(summary) = outcome else {
            panic!("expected a shadow pass, got {outcome:?}");
        };
        assert_eq!(summary.local_workloads, 3);
        assert_eq!(summary.enforced_policies, 1);
        assert_eq!(summary.bindings, 3);
        assert_eq!(
            h.executor.applied.lock().await.as_slice(),
            [node_removal_ruleset(h.enforcer.table())].as_slice(),
            "shadow only ever removes (leftovers here), never applies policy"
        );
        assert_eq!(h.dns.load().restricted_sources(), 0);
        assert!(h.egress.load().is_empty());
    }

    #[tokio::test]
    async fn a_linked_service_on_another_node_is_allowed_by_its_remote_address() {
        let mut h = harness(NodeMode::Enforce);
        // service/5 runs only on a worker: no local container, one address
        // the control plane recorded for it.
        if let Ok(mut containers) = h.instances.containers.lock() {
            containers.retain(|c| c.name != "app-2");
        }
        let mut snapshot = h.policies.snapshot().await.unwrap();
        snapshot.container_names.clear();
        snapshot.remote = IdentityMap::new().with(service(5), "172.20.5.42".parse().unwrap());
        h.policies.set_snapshot(snapshot);
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Applied(_)
        ));
        let applied = h.executor.applied.lock().await;
        assert!(
            applied[0].contains("172.20.5.42"),
            "the remote service's address backs the link allow: {}",
            applied[0]
        );
    }

    /// The elements line of an nft set in a ruleset, e.g.
    /// `elements = { 10.0.0.1, 10.0.0.2 }`.
    fn set_elements(ruleset: &str, set: &str) -> String {
        let header = format!("set {set} {{");
        let start = ruleset
            .find(&header)
            .unwrap_or_else(|| panic!("set {set} missing:\n{ruleset}"));
        let rest = &ruleset[start..];
        let end = rest.find("\t}").unwrap_or(rest.len());
        rest[..end]
            .lines()
            .find(|l| l.contains("elements"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    #[tokio::test]
    async fn a_service_with_local_and_remote_members_is_matched_at_every_member() {
        // service/5 has a local member (app-2, 10.237.1.3) and a data member
        // on a worker the control plane recorded at 172.20.5.42.
        let mut h = harness(NodeMode::Enforce);
        let mut snapshot = h.policies.snapshot().await.unwrap();
        snapshot.remote = IdentityMap::new().with(service(5), "172.20.5.42".parse().unwrap());
        // A second local sandbox with an open egress profile denies service/5.
        snapshot.policies.push(WorkloadPolicy {
            subject: sandbox(2),
            egress: Profile::Open,
            ingress: Profile::Open,
            rules: vec![Rule {
                id: 2,
                direction: Direction::Egress,
                peer: Peer::Workload(service(5)),
                ports: None,
                protocol: L4Protocol::Any,
                action: Action::Deny,
                origin: Origin::Operator,
            }],
        });
        if let Ok(mut containers) = h.instances.containers.lock() {
            containers.push(container(
                4,
                &[(labels::KIND, "sandbox"), (labels::OWNER_ID, "2")],
                "10.237.1.5",
            ));
        }
        h.policies.set_snapshot(snapshot);
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Applied(_)
        ));
        let applied = h.executor.applied.lock().await;
        let ruleset = &applied[0];
        assert_eq!(
            set_elements(ruleset, "w_service_5_v4"),
            "elements = { 10.237.1.3, 172.20.5.42 }",
            "both the local and the remote member are in the peer set:\n{ruleset}"
        );
        // The restricted client's allow and the open profile's deny both
        // match through that set.
        assert!(
            ruleset.contains("ip daddr @w_service_5_v4 tcp dport 5432 return"),
            "{ruleset}"
        );
        assert!(
            ruleset.contains("ip daddr @w_service_5_v4 counter reject"),
            "{ruleset}"
        );
    }

    #[tokio::test]
    async fn a_remote_record_for_an_address_another_local_container_holds_is_ignored() {
        let mut h = harness(NodeMode::Enforce);
        let mut snapshot = h.policies.snapshot().await.unwrap();
        // 10.237.1.2 is sandbox/1 on this node: a record claiming it for
        // service/5 is stale and must not hand sandbox/1 that identity.
        // 10.237.1.3 repeats service/5's own local member and is kept.
        snapshot.remote = IdentityMap::new()
            .with(service(5), "10.237.1.2".parse().unwrap())
            .with(service(5), "10.237.1.3".parse().unwrap())
            .with(service(5), "172.20.5.42".parse().unwrap());
        let plan = h.enforcer.plan(&snapshot).await.unwrap();
        let addrs: Vec<String> = plan
            .identities
            .addresses(&service(5))
            .map(|a| a.to_string())
            .collect();
        assert_eq!(addrs, ["10.237.1.3", "172.20.5.42"]);
        assert_eq!(
            plan.identities.owner_of("10.237.1.2".parse().unwrap()),
            Some(sandbox(1))
        );
    }

    #[tokio::test]
    async fn enforce_applies_once_and_only_again_on_change() {
        let mut h = harness(NodeMode::Enforce);
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Applied(_)
        ));
        {
            let applied = h.executor.applied.lock().await;
            assert_eq!(applied.len(), 1);
            let ruleset = &applied[0];
            assert!(ruleset.contains("table inet temps_netpol_t {"), "{ruleset}");
            assert!(
                ruleset.contains("table bridge temps_netpol_t {"),
                "{ruleset}"
            );
            assert!(
                ruleset.contains("10.237.1.53"),
                "system peer allowed: {ruleset}"
            );
            assert!(ruleset.contains("iifname \"veth1\""), "{ruleset}");
        }
        assert_eq!(h.dns.load().restricted_sources(), 1);
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Unchanged {
                mode: NodeMode::Enforce,
                ..
            }
        ));
        assert_eq!(h.executor.applied.lock().await.len(), 1);
        assert_eq!(
            h.ports.calls.load(Ordering::Relaxed),
            1,
            "ports cached by endpoint"
        );

        // A new instance changes the transaction.
        if let Ok(mut containers) = h.instances.containers.lock() {
            containers.push(container(
                4,
                &[(labels::KIND, "sandbox"), (labels::OWNER_ID, "2")],
                "10.237.1.5",
            ));
        }
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Applied(_)
        ));
        assert_eq!(h.executor.applied.lock().await.len(), 2);
        assert_eq!(h.ports.calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn leaving_enforce_removes_both_tables() {
        let mut h = harness(NodeMode::Enforce);
        h.enforcer.reconcile().await.unwrap();
        h.policies.set_mode(NodeMode::Legacy);
        assert_eq!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Removed {
                mode: NodeMode::Legacy
            }
        );
        let applied = h.executor.applied.lock().await;
        assert_eq!(
            applied.last().map(String::as_str),
            Some(node_removal_ruleset(h.enforcer.table()).as_str())
        );
        assert_eq!(h.dns.load().restricted_sources(), 0);
        drop(applied);
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
    }

    #[tokio::test]
    async fn a_restarted_legacy_node_clears_tables_a_previous_process_left() {
        // The previous process enforced; this one starts with the node
        // already switched back to legacy and must not keep its rules.
        let mut h = harness(NodeMode::Legacy);
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        {
            let applied = h.executor.applied.lock().await;
            assert_eq!(
                applied.as_slice(),
                [node_removal_ruleset(h.enforcer.table())].as_slice(),
                "the first legacy pass clears both tables"
            );
        }
        // Later legacy passes do not touch the kernel again.
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        assert_eq!(h.executor.applied.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn a_failed_leftover_clear_is_retried_on_the_next_pass() {
        let mut h = harness(NodeMode::Legacy);
        h.executor.fail_next.store(true, Ordering::Relaxed);
        assert!(h.enforcer.reconcile().await.is_err());
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        assert_eq!(
            h.executor.applied.lock().await.as_slice(),
            [node_removal_ruleset(h.enforcer.table())].as_slice()
        );
    }

    #[tokio::test]
    async fn a_restarted_shadow_node_clears_leftover_tables_before_shadowing() {
        let mut h = harness(NodeMode::Shadow);
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Shadowed(_)
        ));
        let applied = h.executor.applied.lock().await;
        assert_eq!(
            applied.as_slice(),
            [node_removal_ruleset(h.enforcer.table())].as_slice()
        );
    }

    #[tokio::test]
    async fn enforce_to_shadow_removes_then_shadows() {
        let mut h = harness(NodeMode::Enforce);
        h.enforcer.reconcile().await.unwrap();
        h.policies.set_mode(NodeMode::Shadow);
        assert!(matches!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Shadowed(_)
        ));
        let applied = h.executor.applied.lock().await;
        assert_eq!(applied.len(), 2);
        assert!(applied[1].contains("delete table bridge temps_netpol_t"));
    }

    #[tokio::test]
    async fn restricted_workloads_may_reach_named_system_containers_only() {
        let h = harness(NodeMode::Enforce);
        let mut sidecar = container(4, &[], "10.237.1.9");
        sidecar.name = "temps-sandbox-egress-proxy-v2-abc".into();
        if let Ok(mut containers) = h.instances.containers.lock() {
            containers.push(sidecar);
        }
        let mut snapshot = h.policies.snapshot().await.unwrap();
        snapshot.system_containers = vec![SystemContainer {
            name_prefix: "temps-sandbox-egress-proxy-".into(),
            ports: Some(PortRange::single(3128)),
            protocol: L4Protocol::Tcp,
        }];
        let mut enforcer = h.enforcer;
        let plan = enforcer.plan(&snapshot).await.unwrap();
        assert!(
            plan.ruleset.contains("10.237.1.9") && plan.ruleset.contains("3128"),
            "the sidecar's observed address is allowed on its port:\n{}",
            plan.ruleset
        );
        snapshot.system_containers.clear();
        let plan = enforcer.plan(&snapshot).await.unwrap();
        assert!(
            !plan.ruleset.contains("10.237.1.9"),
            "and not without the entry"
        );
    }

    #[derive(Default)]
    struct DatapathRecord {
        opened: usize,
        applies: Vec<(crate::ebpf::DatapathPlan, Vec<CgroupTarget>)>,
        modes: Vec<NodeMode>,
    }

    struct RecordingDatapath(Arc<std::sync::Mutex<DatapathRecord>>);

    impl CgroupDatapath for RecordingDatapath {
        fn set_mode(&mut self, mode: NodeMode) -> Result<(), DatapathError> {
            self.0.lock().unwrap().modes.push(mode);
            Ok(())
        }
        fn apply(
            &mut self,
            plan: &crate::ebpf::DatapathPlan,
            targets: &[CgroupTarget],
        ) -> Result<crate::ebpf::ApplyStats, DatapathError> {
            self.0
                .lock()
                .unwrap()
                .applies
                .push((plan.clone(), targets.to_vec()));
            Ok(crate::ebpf::ApplyStats::default())
        }
        fn counters(&self) -> Result<crate::ebpf::Counters, DatapathError> {
            Ok(Default::default())
        }
        fn drain_samples(&mut self, _max: usize) -> Vec<temps_netpol_ebpf_common::Sample> {
            Vec::new()
        }
    }

    /// pid → cgroup path; a directory's id is a hash of its path.
    struct FakeHost {
        cgroups: std::sync::Mutex<HashMap<i64, String>>,
    }

    impl CgroupHost for FakeHost {
        fn cgroup_of(&self, pid: i64) -> Option<String> {
            self.cgroups.lock().unwrap().get(&pid).cloned()
        }
        fn cgroup_id(&self, dir: &std::path::Path) -> Option<u64> {
            let mut hasher = DefaultHasher::new();
            dir.hash(&mut hasher);
            Some(hasher.finish())
        }
        fn addresses(&self) -> Vec<IpAddr> {
            vec!["10.77.0.10".parse().unwrap()]
        }
    }

    const SERVICE_SLICE: &str = "/temps.slice/temps-service.slice/temps-service-5.slice";

    fn with_datapath(
        h: Harness,
        pin_dir: &str,
    ) -> (
        Harness,
        Arc<std::sync::Mutex<DatapathRecord>>,
        Arc<FakeHost>,
    ) {
        let record = Arc::new(std::sync::Mutex::new(DatapathRecord::default()));
        let host = Arc::new(FakeHost {
            cgroups: std::sync::Mutex::new(HashMap::from([
                (1, "/system.slice/docker-c1.scope".to_string()),
                (2, format!("{SERVICE_SLICE}/docker-c2.scope")),
                (3, "/system.slice/docker-c3.scope".to_string()),
            ])),
        });
        let opener_record = record.clone();
        let opener: DatapathOpener = Box::new(move || {
            opener_record.lock().unwrap().opened += 1;
            Ok(Box::new(RecordingDatapath(opener_record.clone())) as Box<dyn CgroupDatapath>)
        });
        let config = DatapathConfig {
            pin_dir: pin_dir.into(),
            ..Default::default()
        };
        let Harness {
            enforcer,
            executor,
            policies,
            instances,
            ports,
            dns,
            egress,
        } = h;
        let enforcer = enforcer
            .with_datapath(opener, &config)
            .with_cgroup_host(host.clone());
        (
            Harness {
                enforcer,
                executor,
                policies,
                instances,
                ports,
                dns,
                egress,
            },
            record,
            host,
        )
    }

    const NO_PINS: &str = "/nonexistent/temps-netpol-test-pins";

    #[tokio::test]
    async fn a_pass_reports_only_the_workloads_it_observed_locally() {
        let mut h = harness(NodeMode::Shadow);
        let remote = WorkloadRef::new(WorkloadKind::Deployment, 40);
        let mut snapshot = harness_snapshot();
        snapshot.remote = IdentityMap::new().with(remote, "10.202.9.2".parse().unwrap());
        h.policies.set_snapshot(snapshot);
        h.enforcer.reconcile().await.unwrap();
        let observed = h.policies.last_observed();
        let mut workloads: Vec<WorkloadRef> = observed.workloads().copied().collect();
        workloads.sort();
        let mut expected = vec![
            sandbox(1),
            service(5),
            WorkloadRef::new(WorkloadKind::Deployment, 7),
        ];
        expected.sort();
        assert_eq!(workloads, expected, "remote peers are never reported back");
        assert_eq!(
            observed.addresses(&sandbox(1)).collect::<Vec<_>>(),
            ["10.237.1.2".parse::<IpAddr>().unwrap()]
        );
    }

    #[tokio::test]
    async fn the_datapath_identifies_scopes_and_slices_and_skips_an_unchanged_pass() {
        let (mut h, record, _) = with_datapath(harness(NodeMode::Enforce), NO_PINS);
        assert!(h.enforcer.uses_datapath());
        let outcome = h.enforcer.reconcile().await.unwrap();
        assert!(matches!(outcome, PassOutcome::Applied(_)), "{outcome:?}");
        {
            let record = record.lock().unwrap();
            assert_eq!(record.opened, 1);
            assert_eq!(record.modes, vec![NodeMode::Enforce]);
            let (plan, targets) = &record.applies[0];
            let by_workload: HashMap<WorkloadRef, &CgroupTarget> =
                targets.iter().map(|t| (t.workload, t)).collect();
            assert_eq!(
                by_workload[&sandbox(1)].attach.as_deref(),
                Some(std::path::Path::new(
                    "/sys/fs/cgroup/system.slice/docker-c1.scope"
                )),
                "a container outside temps.slice gets the programs on its scope"
            );
            assert_eq!(
                by_workload[&service(5)].attach,
                None,
                "a workload slice is covered by the temps.slice attachment"
            );
            assert!(by_workload.contains_key(&WorkloadRef::new(WorkloadKind::Deployment, 7)));
            assert!(
                plan.ipcache
                    .contains_key(&Cidr::host("10.77.0.10".parse().unwrap())),
                "the node's own address is a system peer"
            );
        }
        {
            let applied = h.executor.applied.lock().await;
            assert_eq!(
                applied.as_slice(),
                [node_removal_ruleset(h.enforcer.table())],
                "nftables only gets the one-time removal of an earlier version's tables"
            );
        }
        assert!(
            h.dns.load().restricted_sources() > 0,
            "enforce publishes the DNS policy"
        );

        let outcome = h.enforcer.reconcile().await.unwrap();
        assert!(
            matches!(outcome, PassOutcome::Unchanged { .. }),
            "{outcome:?}"
        );
        assert_eq!(record.lock().unwrap().applies.len(), 1);
    }

    #[tokio::test]
    async fn legacy_never_opens_the_datapath_unless_a_previous_process_left_pins() {
        let (mut h, record, _) = with_datapath(harness(NodeMode::Legacy), NO_PINS);
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        assert_eq!(record.lock().unwrap().opened, 0);
        assert_eq!(h.instances.observed.load(Ordering::Relaxed), 0);

        // Pins exist: a previous process enforced, so legacy is set once.
        let pins = std::env::temp_dir().join(format!("temps-netpol-pins-{}", std::process::id()));
        std::fs::create_dir_all(&pins).unwrap();
        let (mut h, record, _) = with_datapath(harness(NodeMode::Legacy), pins.to_str().unwrap());
        let outcome = h.enforcer.reconcile().await.unwrap();
        let _ = std::fs::remove_dir_all(&pins);
        assert_eq!(
            outcome,
            PassOutcome::Removed {
                mode: NodeMode::Legacy
            }
        );
        assert_eq!(record.lock().unwrap().modes, vec![NodeMode::Legacy]);
        assert_eq!(h.enforcer.reconcile().await.unwrap(), PassOutcome::Legacy);
        assert_eq!(record.lock().unwrap().modes.len(), 1);
    }

    #[tokio::test]
    async fn leaving_enforce_switches_the_datapath_to_legacy_and_shadow_reapplies() {
        let (mut h, record, _) = with_datapath(harness(NodeMode::Enforce), NO_PINS);
        h.enforcer.reconcile().await.unwrap();
        h.policies.set_mode(NodeMode::Shadow);
        let outcome = h.enforcer.reconcile().await.unwrap();
        assert!(matches!(outcome, PassOutcome::Shadowed(_)), "{outcome:?}");
        assert_eq!(
            h.dns.load().restricted_sources(),
            0,
            "shadow keeps DNS open"
        );
        h.policies.set_mode(NodeMode::Legacy);
        assert_eq!(
            h.enforcer.reconcile().await.unwrap(),
            PassOutcome::Removed {
                mode: NodeMode::Legacy
            }
        );
        assert_eq!(
            record.lock().unwrap().modes,
            vec![NodeMode::Enforce, NodeMode::Shadow, NodeMode::Legacy]
        );
    }

    #[tokio::test]
    async fn a_cgroup_shared_by_two_workloads_identifies_neither() {
        let (mut h, record, host) = with_datapath(harness(NodeMode::Enforce), NO_PINS);
        host.cgroups
            .lock()
            .unwrap()
            .insert(3, format!("{SERVICE_SLICE}/docker-c3.scope"));
        let outcome = h.enforcer.reconcile().await.unwrap();
        let PassOutcome::Applied(summary) = outcome else {
            panic!("{outcome:?}");
        };
        assert!(
            summary
                .warnings
                .iter()
                .any(|w| w.contains("neither is identified")),
            "{:?}",
            summary.warnings
        );
        let record = record.lock().unwrap();
        let (_, targets) = &record.applies[0];
        assert!(
            targets.iter().all(|t| t.workload == sandbox(1)),
            "{targets:?}"
        );
    }

    #[tokio::test]
    async fn a_restarted_container_is_found_again_through_a_new_pid() {
        let (mut h, record, host) = with_datapath(harness(NodeMode::Enforce), NO_PINS);
        h.enforcer.reconcile().await.unwrap();
        // The scope of c1 is recreated: the cached PID now reads nothing,
        // and the new one points at a new scope.
        {
            let mut cgroups = host.cgroups.lock().unwrap();
            cgroups.remove(&1);
            cgroups.insert(1, "/system.slice/docker-c1b.scope".to_string());
        }
        h.enforcer.reconcile().await.unwrap();
        let record = record.lock().unwrap();
        let (_, targets) = record.applies.last().unwrap();
        assert!(targets.iter().any(|t| t.attach.as_deref()
            == Some(std::path::Path::new(
                "/sys/fs/cgroup/system.slice/docker-c1b.scope"
            ))));
    }

    #[test]
    fn a_node_policy_survives_json() {
        let mut snapshot = harness_snapshot();
        snapshot
            .remote
            .insert(service(9), "10.202.3.4".parse().unwrap());
        snapshot
            .system_ingress
            .push("10.201.0.0/16".parse().unwrap());
        let policy = NodePolicy {
            mode: NodeMode::Enforce,
            snapshot: Some(snapshot.clone()),
        };
        let json = serde_json::to_string(&policy).unwrap();
        let back: NodePolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mode, NodeMode::Enforce);
        let back = back.snapshot.unwrap();
        assert_eq!(back.remote, snapshot.remote);
        assert_eq!(back.policies, snapshot.policies);
        assert_eq!(back.system_peers, snapshot.system_peers);
        assert_eq!(back.system_ingress, snapshot.system_ingress);
        assert_eq!(back.container_names, snapshot.container_names);
        // Fields a newer control plane adds are tolerated, missing ones
        // default.
        let minimal: NodePolicy =
            serde_json::from_str(r#"{"mode":"shadow","snapshot":{}}"#).unwrap();
        assert_eq!(minimal.mode, NodeMode::Shadow);
        assert!(minimal.snapshot.unwrap().policies.is_empty());
    }
}
