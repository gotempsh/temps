// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A *Make it reachable* run: the plan's steps in order, every resource it
//! creates journaled, and undo of exactly those, newest first (ADR 064 D4).
//!
//! The steps used to run in the browser (`web/src/components/nodes/
//! ingress-setup.ts`); here they survive a closed tab and serve the CLI and
//! AI tools too. The run talks to the rest of Temps only through [`RunOps`],
//! and reports through [`RunSink`] after every change, so the bookkeeping is
//! tested without a database or Cloudflare.
//!
//! Crash safety: before a call that creates something, the run writes a
//! `pending` [`JournalEntry`] whose key ([`Intent::key`]) is enough to find
//! the resource again; after it, `done` with the resource. If the pending
//! write fails, the call is not made. A retry or a resume reuses the key
//! and looks the resource up before creating it again, and so does the
//! service's reconciler for a run Temps stopped in the middle of.
//!
//! - A step that fails undoes what the run created and ends it `failed`,
//!   as the browser flow did.
//! - A step that waits too long (DNS, publication, reachability) ends it
//!   `waiting` with everything kept: nothing is wrong, it is not live yet.
//! - Undo never touches what the run found already there; it is not in the
//!   journal. A hostname choice the run changed is put back as it was.
//!
//! Records someone else may change (ADR 064 D4, third review): before a DNS
//! write the run reads the record and compares it with what the plan saw;
//! changed, it writes nothing and stops in `conflict` naming the new value.
//! A change to an existing record journals the value read just before the
//! write and the value it intends, and stamps the run's id into the record's
//! ownership marker. A lookup that misses proves the resource absent only
//! where the lookup is authoritative ([`LookupAuthority`]); otherwise the
//! entry stays pending and is never created twice. Undo puts a changed
//! record back only while it still holds exactly what this run wrote, and
//! stops at anything it cannot account for: nothing older is undone past
//! it until the user decides ([`decide`]).

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::model::{
    AddressAudience, AddressReachability, Decision, DnsRecordState, Intent, JournalEntry,
    JournalState, LookupAuthority, PlanOptionKind, PlanStep, PlannedRecord, PreviousChoice,
    ReachState, RunResource, RunState, RunStep, RunStepStatus, StepKind, UndoResult,
};
use super::plan::NamedId;
use super::token_check::TokenCheckResult;
use crate::tailscale::{self, Ownership, ServiceDefinition, ServiceHost, TailscaleCredential};
use temps_core::tailnet::messages as tailnet_messages;
use temps_entities::tailnet_serve_entries::{REPORT_APPLIED, REPORT_CONFLICT, REPORT_ERROR};

/// A managed zone's state under one DNS provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneState {
    pub auto_manage: bool,
    pub verified: bool,
}

/// A tunnel as the run sees it while waiting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelStatus {
    pub status: String,
    pub last_error: Option<String>,
    /// `(node id, state, ready connections, last error)` per replica.
    pub replicas: Vec<(i32, String, Option<i32>, Option<String>)>,
}

/// The worker a run has serve an address through tailscaled (ADR 065 D4),
/// resolved from the plan when the run starts. The address id is known
/// only once the address is attached, so the step adds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServeTarget {
    pub node_id: i32,
    pub node_name: String,
    /// `tailscale_serve` or `tailscale_service`.
    pub via: String,
    /// The `svc:` name, for a Service.
    #[serde(default)]
    pub service: Option<String>,
    /// The tailnet, for a Service's advisory lock.
    #[serde(default)]
    pub tailnet: Option<String>,
}

/// One `tailnet_serve_entries` row a run asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeWish {
    pub target: ServeTarget,
    pub address_id: String,
    pub hostname: String,
    /// The Service's TailVIPs, as the run journaled them, for the agent's
    /// MagicDNS check (ADR 065 D5).
    pub service_addrs: Vec<String>,
}

/// The Tailscale Service a run defines (ADR 065 D4), resolved from the plan
/// when the run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePlan {
    /// `svc:myapp`.
    pub name: String,
    /// The MagicDNS suffix (`example.ts.net`): the Service's lock and the
    /// messages.
    pub tailnet: String,
    /// The worker tag its definition is for.
    pub tag: String,
    /// ADR 031's install id; the marker is `temps:<install>:<this run>`.
    pub install_id: String,
    /// What the plan read at the name; `None` when it was absent.
    pub seen: Option<ServiceDefinition>,
    /// The name a pasted OAuth client is saved under.
    #[serde(default)]
    pub credential_name: String,
}

/// A Serve entry naming a Service, with what identifies its worker in the
/// tailnet (ADR 065 D4): the only hosts that Service may have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEntryHost {
    pub node_id: i32,
    pub node_name: String,
    pub address_id: String,
    pub hostname: String,
    /// The worker's `Self.ID`, as it last reported it.
    pub tailscale_node_id: Option<String>,
    /// Its `Self.DNSName`.
    pub dns_name: Option<String>,
    pub removing: bool,
}

impl ServiceEntryHost {
    fn hosts(&self, host: &ServiceHost) -> bool {
        host.is(self.tailscale_node_id.as_deref(), self.dns_name.as_deref())
    }
}

/// What a confirmed Service delete did (ADR 065 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceDeletion {
    Deleted,
    /// It was gone already.
    Gone,
    /// A check under the lock no longer held: why it was kept.
    Kept(String),
}

/// The hosts advertising `name` that no Serve entry of `address_id` for it
/// accounts for, by name (ADR 065 D4): never accepted as the new truth.
pub fn unrecorded_hosts(
    hosts: &[ServiceHost],
    entries: &[ServiceEntryHost],
    address_id: Option<&str>,
) -> Vec<String> {
    hosts
        .iter()
        .filter(|host| {
            !entries
                .iter()
                .filter(|e| address_id.is_none_or(|a| e.address_id == a))
                .any(|e| e.hosts(host))
        })
        .map(ServiceHost::short_name)
        .collect()
}

/// D4's message for hosts no Temps setup configured.
pub(crate) fn unrecorded_message(service: &str, hosts: &[String]) -> String {
    format!(
        "`{service}` is also advertised by {}, which no Temps setup configured. Someone may \
         have created the same name while Temps did; check it before approving.",
        hosts
            .iter()
            .map(|h| format!("`{h}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Whether `addrs` is non-empty and every one is a Tailscale address.
fn tailscale_addrs(addrs: &[String]) -> bool {
    !addrs.is_empty()
        && addrs.iter().all(|a| {
            a.parse::<IpAddr>()
                .is_ok_and(temps_core::tailnet::is_tailscale_ip)
        })
}

/// What claiming a Serve entry found under its advisory lock (ADR 065 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeClaim {
    /// No row: the run created it, and its `created` reference.
    Created { entry_id: i32 },
    /// The same wish: the run added a `found` reference, nothing else.
    Found { entry_id: i32 },
    /// Another wish holds the slot: what the worker serves now, in words.
    Differs(String),
    /// The row is `removing`: never revived; wait for the agent.
    Removing,
}

/// A Serve entry and what its worker last said about it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServeEntryStatus {
    pub entry_id: i32,
    pub removing: bool,
    /// `applied`, `conflict` or `error`.
    pub report: Option<String>,
    /// What holds the slot, or the error.
    pub detail: Option<String>,
    pub applied_etag: Option<String>,
    /// The worker's tailscaled problem from its last report, if any.
    pub worker_error: Option<String>,
    /// The node is no longer part of the cluster: no agent will report.
    pub worker_gone: bool,
    /// Funnel is on for the entry's name, or reached its socket.
    pub funnel: bool,
}

/// What dropping a run's reference to a Serve entry left (ADR 065 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeRelease {
    /// The run held no reference: nothing to do.
    Gone,
    /// Other runs still reference it, so the worker keeps serving: why.
    Kept(String),
    /// The agent wrote nothing (it reported `conflict`): the row was
    /// deleted without telling the agent anything.
    Deleted,
    /// The row is `removing`: the agent takes its own handler off.
    Removing,
}

/// Everything a run does to the rest of Temps.
#[async_trait]
pub trait RunOps: Send + Sync {
    async fn check_token(&self, token: &str, zone: &str) -> TokenCheckResult;
    /// Finds what `intent` describes, if it exists: how a pending journal
    /// entry is resolved.
    async fn find(&self, intent: &Intent) -> Result<Option<RunResource>, String>;
    /// Saves a Cloudflare DNS provider under `name`; returns its id.
    async fn save_dns_provider(&self, name: &str, zone: &str, token: &str) -> Result<i32, String>;
    async fn delete_dns_provider(&self, id: i32) -> Result<(), String>;
    async fn managed_zone(&self, provider_id: i32, zone: &str)
        -> Result<Option<ZoneState>, String>;
    async fn add_managed_zone(&self, provider_id: i32, zone: &str) -> Result<(), String>;
    async fn set_auto_manage(&self, provider_id: i32, zone: &str, on: bool) -> Result<(), String>;
    async fn verify_zone(&self, provider_id: i32, zone: &str) -> Result<bool, String>;
    async fn remove_managed_zone(&self, provider_id: i32, zone: &str) -> Result<(), String>;
    /// Creates a tunnel that publishes only the hostnames chosen for it.
    /// On failure, the id of a tunnel that was saved anyway.
    async fn create_tunnel(
        &self,
        name: &str,
        provider_id: i32,
        domain: &str,
        node_ids: &[i32],
    ) -> Result<(i32, TunnelStatus), (Option<i32>, String)>;
    async fn tunnel_status(&self, id: i32) -> Result<TunnelStatus, String>;
    /// Marks the connector `id` named `name` `deleting`, so sync never
    /// recreates it; returns its provider tunnel id. `Ok(None)` when it is
    /// gone already.
    async fn mark_tunnel_deleting(&self, id: i32, name: &str) -> Result<Option<String>, String>;
    /// Every hostname connector `id` publishes, or sync would publish
    /// through it now (ADR 058 D9, `auto_publish` included).
    async fn tunnel_hostnames(&self, id: i32) -> Result<Vec<String>, String>;
    /// Removes a `deleting` connector: sync's own records for it, its
    /// tunnel (when it is still `tunnel_id`), its row. Gone already is
    /// done.
    async fn delete_tunnel(&self, id: i32, tunnel_id: Option<&str>) -> Result<(), String>;
    /// The custom domain with this hostname: `(id, environment id)`.
    async fn find_custom_domain(&self, host: &str) -> Result<Option<(i32, i32)>, String>;
    async fn attach_domain(
        &self,
        project_id: i32,
        environment_id: i32,
        host: &str,
    ) -> Result<i32, String>;
    async fn detach_domain(&self, id: i32) -> Result<(), String>;
    /// See `IngressConnectorService::hostname_choice`.
    async fn hostname_choice(&self, host: &str) -> Result<Option<Option<i32>>, String>;
    async fn route_hostname(&self, host: &str, connector_id: i32) -> Result<(), String>;
    /// Puts a hostname's choice back as it was before the run.
    async fn restore_hostname_choice(
        &self,
        host: &str,
        previous: PreviousChoice,
    ) -> Result<(), String>;
    /// ADR 058 D9's state and detail.
    async fn hostname_state(&self, host: &str) -> Result<(String, String), String>;
    /// The `record_type` record at `host` as its DNS provider has it now.
    async fn read_dns_record(
        &self,
        host: &str,
        record_type: &str,
    ) -> Result<Option<DnsRecordState>, String>;
    /// Creates the record, or changes the one Temps already manages, to
    /// `content`, with `run_id` in its ownership marker. Returns its label.
    async fn write_dns_record(
        &self,
        host: &str,
        record_type: &str,
        content: &str,
        run_id: Option<i32>,
    ) -> Result<String, WriteError>;
    async fn remove_dns_record(
        &self,
        host: &str,
        label: &str,
        record_type: &str,
    ) -> Result<(), String>;
    async fn certificate_exists(&self, host: &str) -> Result<bool, String>;
    /// Creates the certificate's record, validated by `challenge`
    /// (`http-01` or `dns-01`); issuing it is [`Self::finish_certificate`].
    async fn start_certificate(&self, host: &str, challenge: &str) -> Result<(), String>;
    async fn finish_certificate(&self, host: &str, email: &str) -> Result<(), String>;
    async fn delete_certificate(&self, host: &str) -> Result<(), String>;
    async fn resolve(&self, host: &str) -> Vec<IpAddr>;
    async fn reachability(&self, host: &str) -> AddressReachability;
    /// Who `host` is for now, on the environment it is attached to.
    async fn address_audience(&self, host: &str) -> Result<Option<AddressAudience>, String>;
    /// Sets who `host` is for (ADR 064 D5).
    async fn set_address_audience(
        &self,
        host: &str,
        audience: &AddressAudience,
    ) -> Result<(), String>;
    /// Creates the `(node, address)` Serve entry with this run's reference,
    /// or adds a reference to the same one, in one transaction holding the
    /// entry's advisory lock (and the Service's first) (ADR 065 D4).
    async fn claim_serve_entry(&self, run_id: i32, wish: &ServeWish) -> Result<ServeClaim, String>;
    /// This run's reference to the `(node, address)` entry: `(entry id,
    /// created)`.
    async fn serve_reference(
        &self,
        run_id: i32,
        node_id: i32,
        address_id: &str,
    ) -> Result<Option<(i32, bool)>, String>;
    async fn serve_entry(
        &self,
        node_id: i32,
        address_id: &str,
    ) -> Result<Option<ServeEntryStatus>, String>;
    /// Drops this run's reference under the entry's lock; the last one
    /// decides what happens to the entry by the agent's last report.
    async fn release_serve_entry(
        &self,
        run_id: i32,
        node_id: i32,
        address_id: &str,
    ) -> Result<ServeRelease, String>;
    /// Deletes a `removing` entry the user decided to leave on the worker
    /// as it is, so nothing more is asked of the agent.
    async fn forget_serve_entry(&self, entry_id: i32) -> Result<(), String>;
    /// Proves a pasted Tailscale OAuth client's scopes and saves it, sealed,
    /// under `name` (ADR 065 D2); returns its row id.
    async fn save_tailscale_credential(
        &self,
        name: &str,
        credential: &TailscaleCredential,
    ) -> Result<i32, String>;
    /// Removes the credential row unless a Service setup still needs it:
    /// `Some(why)` when it is kept.
    async fn release_tailscale_credential(&self, id: i32) -> Result<Option<String>, String>;
    /// The Service `name` in the cluster's tailnet, as the API has it now.
    async fn read_service(&self, name: &str) -> Result<Option<ServiceDefinition>, String>;
    /// `PUT`, only ever to create (ADR 065 D4).
    async fn create_service(&self, definition: &ServiceDefinition) -> Result<(), WriteError>;
    /// The devices advertising `name`.
    async fn service_hosts(&self, name: &str) -> Result<Vec<ServiceHost>, String>;
    /// Every Serve entry naming `name`, on any node.
    async fn service_entries(&self, name: &str) -> Result<Vec<ServiceEntryHost>, String>;
    /// The Service run `creator_run_id` of this install journaled as
    /// `created`, with when: `None` when no journal records creating it.
    async fn service_creation(
        &self,
        name: &str,
        creator_run_id: i32,
    ) -> Result<Option<(ServiceDefinition, temps_core::DBDateTime)>, String>;
    /// The confirmed delete (ADR 065 D4): under the Service's lock, no
    /// Serve entry names it, no host advertises it, and a read made just
    /// before the `DELETE` still carries this install's marker for
    /// `creator_run_id` and equals `shown`; else it is kept.
    async fn delete_service(
        &self,
        tailnet: &str,
        name: &str,
        creator_run_id: i32,
        shown: &ServiceDefinition,
    ) -> Result<ServiceDeletion, String>;
    async fn sleep(&self, duration: Duration);
}

/// A write that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteError {
    pub message: String,
    /// The provider may have applied it anyway (a timeout, a dropped
    /// connection, a server error); `false` when it refused outright.
    pub maybe_applied: bool,
}

impl WriteError {
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            maybe_applied: false,
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            maybe_applied: true,
        }
    }
}

/// How long the backoff waits after `failures` lookups that could not
/// answer: 30 s doubling, at most 15 minutes.
pub fn lookup_backoff(failures: u32) -> Duration {
    let base = 30u64.saturating_mul(1u64 << failures.saturating_sub(1).min(5));
    Duration::from_secs(base.min(15 * 60))
}

/// After this long an entry Temps still cannot resolve is shown with what
/// to check by hand.
pub const CHECK_BY_HAND_AFTER: Duration = Duration::from_secs(60 * 60);

/// Where a run's progress goes after every change.
#[async_trait]
pub trait RunSink: Send + Sync {
    /// `Err` when it could not be stored. Before a creating call that means
    /// the call is not made.
    async fn save(&self, record: &RunRecord) -> Result<(), String>;
}

/// Where undo journals a step before the provider call it depends on.
#[async_trait]
pub trait JournalSink: Send + Sync {
    async fn save_journal(&self, journal: &[JournalEntry]) -> Result<(), String>;
}

/// Journals through a [`RunSink`], for undo inside a run.
struct RecordJournal<'a> {
    sink: &'a dyn RunSink,
    record: tokio::sync::Mutex<RunRecord>,
}

#[async_trait]
impl JournalSink for RecordJournal<'_> {
    async fn save_journal(&self, journal: &[JournalEntry]) -> Result<(), String> {
        let mut record = self.record.lock().await;
        record.journal = journal.to_vec();
        self.sink.save(&record).await
    }
}

/// How long the run waits for what happens elsewhere.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll: Duration,
    /// For a new tunnel's worker to connect (the browser flow's 3 minutes).
    pub connect: Duration,
    /// For the reconciler to publish a hostname; it runs every 60 s.
    pub publish: Duration,
    /// For a DNS record the user adds.
    pub dns: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            connect: Duration::from_secs(180),
            publish: Duration::from_secs(90),
            dns: Duration::from_secs(600),
        }
    }
}

/// A new tunnel the run creates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTunnel {
    /// `None` until the run saves the pasted token as a provider.
    pub provider_id: Option<i32>,
    /// The name the pasted token's provider is saved under, picked unused
    /// when the run starts so a lookup finds it again.
    #[serde(default)]
    pub provider_name: String,
    pub zone: String,
    pub domain: String,
    /// Unused when the run starts; Cloudflare's tunnel is
    /// `temps-<name>-<install>`.
    pub name: String,
    pub worker: NamedId,
}

/// What a run is asked to do, resolved from the plan when it starts and
/// stored with the run so a resume does the same.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInput {
    pub run_id: i32,
    pub project_id: i32,
    pub environment_id: i32,
    pub environment_name: String,
    pub option: PlanOptionKind,
    pub address: String,
    pub steps: Vec<PlanStep>,
    /// An existing tunnel to publish through.
    pub tunnel: Option<NamedId>,
    pub new_tunnel: Option<NewTunnel>,
    /// The user confirmed changing the hostname's tunnel choice.
    #[serde(default)]
    pub route_confirmed: bool,
    /// In memory for the run only; never stored.
    #[serde(skip)]
    pub token: Option<String>,
    /// A direct ingress and the address DNS points at.
    pub direct: Option<(String, IpAddr)>,
    /// For Let's Encrypt; the signed-in user's.
    pub acme_email: Option<String>,
    /// The `direct` record's type at `address` as the plan saw it: what
    /// the run compares with before it writes (ADR 064 D4).
    #[serde(default)]
    pub dns_before: PlannedRecord,
    /// `dns-01` for a private address no CA can reach; `http-01` otherwise.
    #[serde(default)]
    pub certificate_challenge: Option<String>,
    /// For a private address: who it is for once the run is done, and who
    /// it was for before.
    #[serde(default)]
    pub audience: Option<(AddressAudience, AddressAudience)>,
    /// For a tailnet name: the worker that serves it (ADR 065 D4).
    #[serde(default)]
    pub serve: Option<ServeTarget>,
    /// For a Tailscale Service: what the run defines (ADR 065 D4).
    #[serde(default)]
    pub tailscale_service: Option<ServicePlan>,
    /// A pasted Tailscale OAuth client, in memory for the run only; never
    /// stored.
    #[serde(skip)]
    pub tailscale_credential: Option<TailscaleCredential>,
}

/// The run as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub state: RunState,
    pub steps: Vec<RunStep>,
    pub journal: Vec<JournalEntry>,
    pub undo: Vec<UndoResult>,
    pub error: Option<String>,
    pub live_url: Option<String>,
}

impl RunRecord {
    pub fn new(steps: &[PlanStep]) -> Self {
        Self {
            state: RunState::Running,
            steps: steps
                .iter()
                .map(|s| RunStep {
                    kind: s.kind,
                    label: s.label.clone(),
                    status: RunStepStatus::Pending,
                    detail: None,
                })
                .collect(),
            journal: Vec::new(),
            undo: Vec::new(),
            error: None,
            live_url: None,
        }
    }

    /// What the run created, oldest first.
    pub fn created(&self) -> Vec<RunResource> {
        created(&self.journal)
    }

    /// Whether a journal entry is still `pending`: a call whose outcome is
    /// not known. Undo and resume wait for the reconciler to resolve it.
    pub fn has_pending(&self) -> bool {
        self.journal
            .iter()
            .any(|e| e.state == JournalState::Pending)
    }

    /// Entries waiting for a user's decision.
    pub fn conflicts(&self) -> Vec<&JournalEntry> {
        self.journal
            .iter()
            .filter(|e| e.state == JournalState::Conflict)
            .collect()
    }
}

/// The `done` entries' resources, oldest first.
pub fn created(journal: &[JournalEntry]) -> Vec<RunResource> {
    journal
        .iter()
        .filter(|e| e.state == JournalState::Done)
        .filter_map(|e| e.resource.clone())
        .collect()
}

/// What looking an intent up found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    Made(Box<RunResource>),
    Missing,
    /// It holds something neither the run nor what it replaced had.
    Changed(String),
}

fn describe_record(current: Option<&DnsRecordState>) -> String {
    match current {
        Some(record) if record.managed => format!("{} (set by Temps)", record.describe()),
        Some(record) => format!("{} (not set by Temps)", record.describe()),
        None => "no record".to_string(),
    }
}

/// The record holds what this run wrote: `content`, with its marker.
fn holds_this_run(record: &DnsRecordState, content: &str, run_id: i32) -> bool {
    record.managed && record.content == content && record.run_id == Some(run_id)
}

async fn look(ops: &dyn RunOps, run_id: i32, intent: &Intent) -> Result<Found, String> {
    match intent {
        Intent::DnsRecord {
            hostname,
            record_type,
            ..
        } => Ok(match ops.read_dns_record(hostname, record_type).await? {
            None => Found::Missing,
            Some(record) if record.managed && record.run_id == Some(run_id) => {
                Found::Made(Box::new(RunResource::DnsRecord {
                    hostname: hostname.clone(),
                    label: record.label,
                    record_type: record_type.clone(),
                    content: Some(record.content),
                }))
            }
            Some(other) => Found::Changed(describe_record(Some(&other))),
        }),
        Intent::DnsChange {
            hostname,
            label,
            record_type,
            before,
            after,
        } => Ok(match ops.read_dns_record(hostname, record_type).await? {
            Some(record) if holds_this_run(&record, after, run_id) => {
                Found::Made(Box::new(RunResource::DnsChange {
                    hostname: hostname.clone(),
                    label: label.clone(),
                    record_type: record_type.clone(),
                    before: before.clone(),
                    after: after.clone(),
                }))
            }
            // Still what it was before the write: the write did not happen.
            Some(record) if record.unchanged_from(before) => Found::Missing,
            other => Found::Changed(describe_record(other.as_ref())),
        }),
        // The row and this run's reference to it: Temps' own rows.
        Intent::TailscaleServe {
            node_id,
            node_name,
            address_id,
            hostname,
            ..
        } => Ok(
            match ops.serve_reference(run_id, *node_id, address_id).await? {
                Some((entry_id, created)) => Found::Made(Box::new(RunResource::TailscaleServe {
                    node_id: *node_id,
                    node_name: node_name.clone(),
                    address_id: address_id.clone(),
                    hostname: hostname.clone(),
                    entry_id,
                    created,
                })),
                None => Found::Missing,
            },
        ),
        // Created by this run only when it carries this run's marker.
        Intent::TailscaleService {
            tailnet,
            name,
            marker,
        } => Ok(match ops.read_service(name).await? {
            None => Found::Missing,
            Some(definition) if definition.comment == *marker => {
                Found::Made(Box::new(RunResource::TailscaleService {
                    tailnet: tailnet.clone(),
                    name: name.clone(),
                    created: true,
                    creator_run_id: run_id,
                    definition,
                }))
            }
            Some(definition) => Found::Changed(format!(
                "`{name}` with the comment `{}`, which this run did not write",
                definition.comment
            )),
        }),
        other => Ok(match ops.find(other).await? {
            Some(resource) => Found::Made(Box::new(resource)),
            None => Found::Missing,
        }),
    }
}

/// Where taking a Serve entry off its worker stands (ADR 065 D4): the
/// entry is gone once the agent reported `removed` or `absent`.
async fn serve_removal(ops: &dyn RunOps, resource: &RunResource) -> Result<Found, String> {
    let RunResource::TailscaleServe {
        node_id,
        node_name,
        address_id,
        hostname,
        entry_id,
        ..
    } = resource
    else {
        return Ok(Found::Missing);
    };
    Ok(match ops.serve_entry(*node_id, address_id).await? {
        Some(status) if status.entry_id == *entry_id && status.removing => {
            if status.worker_gone {
                Found::Changed(format!(
                    "`{node_name}` left the cluster while it served `{hostname}`. Remove it \
                     from the tailnet, or run `tailscale serve reset` on it, then confirm"
                ))
            } else if status.report.as_deref() == Some(REPORT_CONFLICT) {
                Found::Changed(
                    status
                        .detail
                        .unwrap_or_else(|| "a Serve entry Temps did not write".to_string()),
                )
            } else {
                Found::Made(Box::new(resource.clone()))
            }
        }
        // Removed, or replaced by a new entry since.
        _ => Found::Missing,
    })
}

/// What resolving a pending entry decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Done,
    Absent,
    /// Temps cannot tell yet.
    Pending,
    Conflict,
}

fn chrono_of(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

/// Look a pending entry up at `now` and record what it found. A miss
/// proves absence only where the lookup is authoritative: at once for
/// Temps' own rows; for a provider's records only once the create call's
/// timeout and a settle delay have passed and a second miss a settle delay
/// after the first confirms it. A lookup that cannot answer backs off.
pub async fn resolve_pending(
    entry: &mut JournalEntry,
    ops: &dyn RunOps,
    run_id: i32,
    now: temps_core::DBDateTime,
) -> Resolution {
    if entry.state != JournalState::Pending {
        return match entry.state {
            JournalState::Done => Resolution::Done,
            JournalState::Absent => Resolution::Absent,
            _ => Resolution::Conflict,
        };
    }
    if entry.next_lookup_at.is_some_and(|at| at > now) {
        return Resolution::Pending;
    }
    // Undo is taking a Serve entry off its worker: done once it is gone.
    if let (true, Some(resource @ RunResource::TailscaleServe { .. })) =
        (entry.deleting, entry.resource.clone())
    {
        return match serve_removal(ops, &resource).await {
            Ok(Found::Missing) => {
                entry.state = JournalState::Absent;
                entry.conflict = None;
                entry.failed_lookups = 0;
                entry.at = now;
                Resolution::Absent
            }
            Ok(Found::Changed(current)) => {
                entry.state = JournalState::Conflict;
                entry.conflict = Some(current);
                entry.at = now;
                Resolution::Conflict
            }
            Ok(Found::Made(_)) => {
                entry.conflict = None;
                Resolution::Pending
            }
            Err(error) => {
                entry.failed_lookups = entry.failed_lookups.saturating_add(1);
                entry.next_lookup_at = Some(now + chrono_of(lookup_backoff(entry.failed_lookups)));
                entry.conflict = Some(format!("The last check could not answer: {error}"));
                Resolution::Pending
            }
        };
    }
    let found = match look(ops, run_id, &entry.intent).await {
        Ok(found) => found,
        Err(error) => {
            entry.failed_lookups = entry.failed_lookups.saturating_add(1);
            entry.next_lookup_at = Some(now + chrono_of(lookup_backoff(entry.failed_lookups)));
            entry.conflict = Some(format!("The last check could not answer: {error}"));
            return Resolution::Pending;
        }
    };
    entry.failed_lookups = 0;
    entry.next_lookup_at = None;
    entry.conflict = None;
    match found {
        Found::Made(resource) => {
            entry.state = JournalState::Done;
            entry.resource = Some(*resource);
            entry.first_miss_at = None;
            entry.at = now;
            Resolution::Done
        }
        Found::Changed(current) => {
            entry.state = JournalState::Conflict;
            entry.conflict = Some(current);
            entry.at = now;
            Resolution::Conflict
        }
        Found::Missing => match entry.intent.authority() {
            LookupAuthority::Immediate => {
                entry.state = JournalState::Absent;
                entry.at = now;
                Resolution::Absent
            }
            LookupAuthority::Settled { timeout, settle } => {
                let since = entry.pending_since.unwrap_or(entry.at);
                if now < since + chrono_of(timeout + settle) {
                    // Too soon for a miss to mean anything.
                    return Resolution::Pending;
                }
                match entry.first_miss_at {
                    None => {
                        entry.first_miss_at = Some(now);
                        Resolution::Pending
                    }
                    Some(first) if now >= first + chrono_of(settle) => {
                        entry.state = JournalState::Absent;
                        entry.at = now;
                        Resolution::Absent
                    }
                    Some(_) => Resolution::Pending,
                }
            }
        },
    }
}

/// What a reconcile pass left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub resolved: usize,
    /// Entries Temps still cannot tell about.
    pub pending: usize,
    pub conflicts: usize,
    /// Pending for more than [`CHECK_BY_HAND_AFTER`]: what to check.
    pub check_by_hand: Vec<String>,
    /// Why the last lookups could not answer.
    pub errors: Vec<String>,
}

/// Resolve every `pending` entry ([`resolve_pending`]). An entry Temps
/// cannot tell about stays pending: it does not guess.
pub async fn reconcile_journal(
    journal: &mut [JournalEntry],
    ops: &dyn RunOps,
    run_id: i32,
    now: temps_core::DBDateTime,
) -> Reconciled {
    let mut out = Reconciled::default();
    for entry in journal.iter_mut() {
        if entry.state != JournalState::Pending {
            out.conflicts += usize::from(entry.state == JournalState::Conflict);
            continue;
        }
        match resolve_pending(entry, ops, run_id, now).await {
            Resolution::Done | Resolution::Absent => out.resolved += 1,
            Resolution::Conflict => {
                out.resolved += 1;
                out.conflicts += 1;
            }
            Resolution::Pending => {
                out.pending += 1;
                if let Some(error) = entry.conflict.clone() {
                    out.errors.push(error);
                }
                let since = entry.pending_since.unwrap_or(entry.at);
                if now - since >= chrono_of(CHECK_BY_HAND_AFTER) {
                    out.check_by_hand.push(entry.intent.check_by_hand());
                }
            }
        }
    }
    out
}

/// Answer a conflict (ADR 064 D4). While undoing, `keep` leaves the record
/// as someone else set it and `overwrite` restores what was there before;
/// otherwise `keep` skips the step and `overwrite` takes the value there
/// now as the one to change. The decision is journaled with the entry.
pub fn decide(
    journal: &mut [JournalEntry],
    key: &str,
    decision: Decision,
    undoing: bool,
) -> Result<(), String> {
    let entry = journal
        .iter_mut()
        .find(|e| e.key == key)
        .ok_or_else(|| format!("This run has no step `{key}`."))?;
    if entry.state != JournalState::Conflict {
        return Err(format!(
            "{} is not waiting for a decision.",
            entry.intent.describe()
        ));
    }
    if undoing
        && decision == Decision::Overwrite
        && matches!(entry.intent, Intent::DnsRecord { .. })
    {
        // Removing a record needs this run's marker on it (ADR 064 D4).
        return Err(format!(
            "Temps never deletes a DNS record it did not write, and {} now holds {}, which this \
             run did not set. Keep it, or remove it yourself at your DNS provider.",
            entry.intent.describe(),
            entry.conflict.as_deref().unwrap_or("something else")
        ));
    }
    if let Intent::TailscaleServe {
        node_name,
        hostname,
        ..
    } = &entry.intent
    {
        // ADR 065 D4: Temps never removes a handler it did not write, so
        // the only answer is to leave the worker as it is and forget the
        // entry; undo then deletes the row and carries on.
        if decision == Decision::Overwrite || !undoing {
            return Err(format!(
                "Temps never changes Serve configuration it did not write, so it cannot take \
                 {hostname} off {node_name} for you. Remove it there yourself, or keep it."
            ));
        }
        entry.at = chrono::Utc::now();
        entry.undo_decision = Some(Decision::Keep);
        entry.state = JournalState::Done;
        return Ok(());
    }
    if let Intent::TailscaleService { name, tailnet, .. } = &entry.intent {
        // ADR 065 D4: Temps never updates a Service, so going forward
        // there is nothing to decide; undo deletes one only when a person
        // confirms the definition they were shown.
        if !undoing {
            return Err(format!(
                "Temps never changes a Tailscale Service, so it cannot take `{name}` over. Check \
                 it in the admin console, then plan again with another name or once it is fixed."
            ));
        }
        entry.at = chrono::Utc::now();
        entry.undo_decision = Some(decision);
        entry.state = match decision {
            Decision::Keep => JournalState::Absent,
            Decision::Overwrite if entry.shown_service.is_some() => JournalState::Done,
            Decision::Overwrite => {
                return Err(format!(
                    "Temps deletes `{name}` from {tailnet} only after showing you what it \
                     deletes, and nothing was shown for it. Undo again to see it."
                ))
            }
        };
        return Ok(());
    }
    entry.at = chrono::Utc::now();
    if undoing {
        entry.undo_decision = Some(decision);
        // A change whose outcome was in conflict before undo started has
        // its before state in the intent: that is what restoring writes.
        if decision == Decision::Overwrite && entry.resource.is_none() && !entry.unwritten {
            if let Intent::DnsChange {
                hostname,
                label,
                record_type,
                before,
                after,
            } = &entry.intent
            {
                entry.resource = Some(RunResource::DnsChange {
                    hostname: hostname.clone(),
                    label: label.clone(),
                    record_type: record_type.clone(),
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
        // Restoring needs what it would restore; a record the run never
        // wrote has nothing to put back, and is left as it is.
        entry.state = match (decision, &entry.resource) {
            (Decision::Overwrite, Some(_)) => JournalState::Done,
            _ => JournalState::Absent,
        };
    } else {
        entry.decision = Some(decision);
        entry.state = JournalState::Absent;
    }
    Ok(())
}

enum Outcome {
    Done(Option<String>),
    Skipped(String),
    Failed(String),
    /// Nothing is wrong, but it is not there yet.
    Stalled(String),
    /// Someone else changed what the step changes: nothing is touched
    /// until a user decides.
    Conflict(String),
}

/// Whether a journaled step has to make its call.
enum Begin {
    /// An earlier attempt created it already.
    Have(Box<RunResource>),
    /// The pending entry is stored; make the call.
    Go,
}

/// Why a journaled step cannot make its call.
enum Hold {
    Failed(String),
    /// An earlier attempt may have made it; Temps cannot tell yet.
    Wait(String),
    Conflict(String),
}

impl From<Hold> for Outcome {
    fn from(hold: Hold) -> Self {
        match hold {
            Hold::Failed(error) => Outcome::Failed(error),
            Hold::Wait(reason) => Outcome::Stalled(reason),
            Hold::Conflict(reason) => Outcome::Conflict(reason),
        }
    }
}

struct Context {
    provider_id: Option<i32>,
    connector_id: Option<i32>,
}

struct Runner<'a> {
    input: &'a RunInput,
    ops: &'a dyn RunOps,
    sink: &'a dyn RunSink,
    timing: Timing,
    record: RunRecord,
    ctx: Context,
}

/// Why a step waits for an earlier attempt's outcome.
fn wait_message(entry: &JournalEntry, now: temps_core::DBDateTime) -> String {
    format!(
        "An earlier attempt may have made {} already, and Temps cannot tell yet{}. It never \
         makes it twice: it checks again every minute; resume once it has.{}",
        entry.intent.describe(),
        entry
            .conflict
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default(),
        if now - entry.pending_since.unwrap_or(entry.at) >= chrono_of(CHECK_BY_HAND_AFTER) {
            format!(" {}", entry.intent.check_by_hand())
        } else {
            String::new()
        }
    )
}

fn conflict_message(entry: &JournalEntry) -> String {
    format!(
        "{} was changed by someone else while this run was working on it: it is now {}. \
         Temps leaves it alone until you decide: keep that value, or let Temps set it.",
        entry.intent.describe(),
        entry.conflict.as_deref().unwrap_or("something else")
    )
}

fn record_type(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(_) => "A",
        IpAddr::V6(_) => "AAAA",
    }
}

impl Runner<'_> {
    /// Progress that is not a journal entry: a failed save is logged, and
    /// the next one carries it.
    async fn save(&self) {
        if let Err(error) = self.sink.save(&self.record).await {
            tracing::warn!(
                run = self.input.run_id,
                "could not save reachability run progress: {error}"
            );
        }
    }

    async fn set(&mut self, index: usize, status: RunStepStatus, detail: Option<String>) {
        if let Some(step) = self.record.steps.get_mut(index) {
            step.status = status;
            step.detail = detail;
        }
        self.save().await;
    }

    fn known(&self, intent: &Intent) -> bool {
        let key = intent.key(self.input.run_id);
        self.record
            .journal
            .iter()
            .any(|e| e.key == key && e.state != JournalState::Absent)
    }

    /// Before a creating call: reuse what an earlier attempt made, else
    /// store a pending entry. Nothing is called when that cannot be stored,
    /// nor while an earlier attempt's outcome is unknown.
    async fn begin(&mut self, intent: &Intent) -> Result<Begin, Hold> {
        let key = intent.key(self.input.run_id);
        let now = chrono::Utc::now();
        match self.record.journal.iter().position(|e| e.key == key) {
            Some(at) => {
                let run_id = self.input.run_id;
                let entry = &mut self.record.journal[at];
                if entry.deleting {
                    return Err(Hold::Failed(format!(
                        "Undo has started removing {}, and a tunnel being removed is never \
                         reused. Undo this run again to finish, then make it reachable again.",
                        intent.describe()
                    )));
                }
                match (entry.state, entry.resource.clone()) {
                    (JournalState::Done, Some(resource)) => {
                        return Ok(Begin::Have(Box::new(resource)))
                    }
                    (JournalState::Pending, _) => {
                        let resolution = resolve_pending(entry, self.ops, run_id, now).await;
                        let entry = entry.clone();
                        self.save().await;
                        match resolution {
                            Resolution::Done => {
                                if let Some(resource) = entry.resource {
                                    return Ok(Begin::Have(Box::new(resource)));
                                }
                            }
                            Resolution::Absent => {}
                            Resolution::Pending => {
                                return Err(Hold::Wait(wait_message(&entry, now)))
                            }
                            Resolution::Conflict => {
                                return Err(Hold::Conflict(conflict_message(&entry)))
                            }
                        }
                    }
                    (JournalState::Conflict, _) => {
                        return Err(Hold::Conflict(conflict_message(entry)))
                    }
                    _ => {}
                }
                let entry = &mut self.record.journal[at];
                entry.intent = intent.clone();
                entry.state = JournalState::Pending;
                entry.resource = None;
                entry.at = now;
                entry.pending_since = Some(now);
                entry.first_miss_at = None;
                entry.failed_lookups = 0;
                entry.next_lookup_at = None;
                entry.conflict = None;
                entry.unwritten = false;
            }
            None => {
                self.record
                    .journal
                    .push(JournalEntry::pending(key.clone(), intent.clone(), now))
            }
        }
        self.sink.save(&self.record).await.map_err(|error| {
            Hold::Failed(format!(
                "Temps could not record that it is about to create {}, so it did not: {error}",
                intent.describe()
            ))
        })?;
        Ok(Begin::Go)
    }

    /// After the call: `Some` what it made, `None` nothing.
    async fn finish(&mut self, key: &str, resource: Option<RunResource>) {
        if let Some(entry) = self.record.journal.iter_mut().find(|e| e.key == key) {
            entry.state = if resource.is_some() {
                JournalState::Done
            } else {
                JournalState::Absent
            };
            entry.resource = resource;
            entry.at = chrono::Utc::now();
        }
        self.save().await;
    }

    /// After a failed call: it may have made something anyway, so look,
    /// unless the provider refused it outright. What cannot be told yet
    /// stays pending; undo stops there and the reconciler resolves it.
    async fn finish_failed(&mut self, intent: &Intent, maybe_applied: bool) {
        let key = intent.key(self.input.run_id);
        if !maybe_applied {
            return self.finish(&key, None).await;
        }
        let run_id = self.input.run_id;
        if let Some(entry) = self.record.journal.iter_mut().find(|e| e.key == key) {
            let resolution = resolve_pending(entry, self.ops, run_id, chrono::Utc::now()).await;
            if resolution == Resolution::Pending {
                tracing::warn!(
                    run = run_id,
                    "could not tell yet whether {} was created",
                    intent.describe()
                );
            }
        }
        self.save().await;
    }

    /// Resolve the pending entry under `key`, if there is one.
    async fn settle(&mut self, key: &str) -> Resolution {
        let run_id = self.input.run_id;
        let Some(entry) = self.record.journal.iter_mut().find(|e| e.key == key) else {
            return Resolution::Absent;
        };
        let resolution = resolve_pending(entry, self.ops, run_id, chrono::Utc::now()).await;
        self.save().await;
        resolution
    }

    /// Point `host` at `ip` (ADR 064 D4): read the record just before
    /// writing, write nothing when it is not what the plan saw, journal a
    /// change with the value it replaces.
    async fn set_dns_record(&mut self, host: &str, ip: IpAddr) -> Outcome {
        let run_id = self.input.run_id;
        let kind = record_type(ip).to_string();
        let content = ip.to_string();
        let key = Intent::DnsRecord {
            hostname: host.to_string(),
            label: String::new(),
            record_type: kind.clone(),
        }
        .key(run_id);
        let done = Outcome::Done(Some(format!("{host} points at {ip}.")));
        let mut decision = None;
        if let Some(entry) = self.record.journal.iter().find(|e| e.key == key).cloned() {
            decision = entry.decision;
            match self.settle(&key).await {
                Resolution::Done => return done,
                Resolution::Absent => {}
                Resolution::Pending => {
                    let entry = self
                        .record
                        .journal
                        .iter()
                        .find(|e| e.key == key)
                        .cloned()
                        .unwrap_or(entry);
                    return Outcome::Stalled(wait_message(&entry, chrono::Utc::now()));
                }
                Resolution::Conflict => {
                    let entry = self
                        .record
                        .journal
                        .iter()
                        .find(|e| e.key == key)
                        .cloned()
                        .unwrap_or(entry);
                    return Outcome::Conflict(conflict_message(&entry));
                }
            }
        }
        if decision == Some(Decision::Keep) {
            return Outcome::Skipped(format!(
                "Left the {kind} record for {host} as someone else set it, as you chose."
            ));
        }
        let current = match self.ops.read_dns_record(host, &kind).await {
            Ok(current) => current,
            Err(error) => {
                return Outcome::Failed(format!(
                    "Temps could not read the {kind} record for {host} just before writing it, \
                     so it wrote nothing: {error} Try again in a minute."
                ))
            }
        };
        let before = if decision == Some(Decision::Overwrite) {
            current.clone()
        } else {
            match (&self.input.dns_before, &current) {
                (PlannedRecord::Absent, None) => None,
                (PlannedRecord::Present { record }, Some(now)) if now.unchanged_from(record) => {
                    Some(now.clone())
                }
                (PlannedRecord::Unknown, _) => {
                    return Outcome::Failed(format!(
                        "When this plan was made Temps could not read the {kind} record for \
                         {host}, so it will not write over one you have not seen. Plan again."
                    ))
                }
                (planned, now) => {
                    let was = match planned {
                        PlannedRecord::Present { record } => describe_record(Some(record)),
                        _ => "no record".to_string(),
                    };
                    let intent = match now {
                        Some(now) => Intent::DnsChange {
                            hostname: host.to_string(),
                            label: now.label.clone(),
                            record_type: kind.clone(),
                            before: now.clone(),
                            after: content.clone(),
                        },
                        None => Intent::DnsRecord {
                            hostname: host.to_string(),
                            label: String::new(),
                            record_type: kind.clone(),
                        },
                    };
                    let mut entry = JournalEntry::pending(key.clone(), intent, chrono::Utc::now());
                    entry.state = JournalState::Conflict;
                    entry.pending_since = None;
                    entry.unwritten = true;
                    entry.conflict = Some(describe_record(now.as_ref()));
                    self.record.journal.retain(|e| e.key != key);
                    self.record.journal.push(entry);
                    self.save().await;
                    return Outcome::Conflict(format!(
                        "The {kind} record for {host} changed at your DNS provider after this \
                         plan was made: it is now {} (the plan saw {was}). Temps wrote \
                         nothing. Keep that value, or let Temps point {host} at {ip}.",
                        describe_record(now.as_ref())
                    ));
                }
            }
        };
        if before.as_ref().is_some_and(|b| b.content == content) {
            return Outcome::Skipped(format!("{host} already points at {ip}."));
        }
        let intent = match &before {
            None => Intent::DnsRecord {
                hostname: host.to_string(),
                label: String::new(),
                record_type: kind.clone(),
            },
            Some(before) => Intent::DnsChange {
                hostname: host.to_string(),
                label: before.label.clone(),
                record_type: kind.clone(),
                before: before.clone(),
                after: content.clone(),
            },
        };
        match self.begin(&intent).await {
            Err(hold) => hold.into(),
            Ok(Begin::Have(_)) => done,
            Ok(Begin::Go) => match self
                .ops
                .write_dns_record(host, &kind, &content, Some(run_id))
                .await
            {
                Ok(label) => {
                    let (resource, detail) = match intent.clone() {
                        Intent::DnsChange {
                            hostname,
                            record_type,
                            before,
                            after,
                            ..
                        } => {
                            let detail =
                                format!("{host} changed from {} to {ip}.", before.describe());
                            (
                                RunResource::DnsChange {
                                    hostname,
                                    label,
                                    record_type,
                                    before,
                                    after,
                                },
                                detail,
                            )
                        }
                        _ => (
                            RunResource::DnsRecord {
                                hostname: host.to_string(),
                                label,
                                record_type: kind.clone(),
                                content: Some(content.clone()),
                            },
                            format!("{host} points at {ip}."),
                        ),
                    };
                    self.finish(&key, Some(resource)).await;
                    Outcome::Done(Some(detail))
                }
                Err(error) => {
                    self.finish_failed(&intent, error.maybe_applied).await;
                    Outcome::Failed(error.message)
                }
            },
        }
    }

    /// The address's id, from what attached it: the journal, else its row.
    async fn address_id(&self) -> Result<String, String> {
        let address = &self.input.address;
        let attached = created(&self.record.journal)
            .into_iter()
            .find_map(|r| match r {
                RunResource::CustomDomain { id, domain } if domain == *address => Some(id),
                _ => None,
            });
        let id = match attached {
            Some(id) => id,
            None => match self.ops.find_custom_domain(address).await? {
                Some((id, environment_id)) if environment_id == self.input.environment_id => id,
                _ => {
                    return Err(format!(
                        "{address} is not attached to {} any more.",
                        self.input.environment_name
                    ))
                }
            },
        };
        Ok(temps_core::worker_ingress::private_address_id(true, id))
    }

    /// ADR 065 D4: declare the worker's Serve entry, referenced by this
    /// run, then wait for its agent to apply it. Done only on `applied`;
    /// the agent never overwrites what it did not write, so neither does
    /// this step.
    async fn configure_serve(&mut self, target: &ServeTarget) -> Outcome {
        let input = self.input;
        let host = input.address.clone();
        let node = target.node_name.clone();
        let address_id = match self.address_id().await {
            Ok(id) => id,
            Err(error) => return Outcome::Failed(error),
        };
        let intent = Intent::TailscaleServe {
            node_id: target.node_id,
            node_name: node.clone(),
            address_id: address_id.clone(),
            hostname: host.clone(),
            service: target.service.clone(),
        };
        match self.begin(&intent).await {
            Err(hold) => return hold.into(),
            Ok(Begin::Have(_)) => {}
            Ok(Begin::Go) => {
                let wish = ServeWish {
                    target: target.clone(),
                    address_id: address_id.clone(),
                    hostname: host.clone(),
                    service_addrs: self.service_resource().map_or_else(Vec::new, |r| match r {
                        RunResource::TailscaleService { definition, .. } => definition.addrs,
                        _ => Vec::new(),
                    }),
                };
                let key = intent.key(input.run_id);
                match self.ops.claim_serve_entry(input.run_id, &wish).await {
                    Ok(claim @ (ServeClaim::Created { .. } | ServeClaim::Found { .. })) => {
                        let (entry_id, created) = match claim {
                            ServeClaim::Created { entry_id } => (entry_id, true),
                            ServeClaim::Found { entry_id } => (entry_id, false),
                            ServeClaim::Differs(_) | ServeClaim::Removing => (0, false),
                        };
                        self.finish(
                            &key,
                            Some(RunResource::TailscaleServe {
                                node_id: target.node_id,
                                node_name: node.clone(),
                                address_id: address_id.clone(),
                                hostname: host.clone(),
                                entry_id,
                                created,
                            }),
                        )
                        .await;
                    }
                    Ok(ServeClaim::Differs(what)) => {
                        self.finish(&key, None).await;
                        return Outcome::Conflict(format!(
                            "{node} already has a Serve entry for {host} that another Temps \
                             setup asked for: {what}. Temps changes nothing there: undo that \
                             setup first, then resume this one."
                        ));
                    }
                    Ok(ServeClaim::Removing) => {
                        self.finish(&key, None).await;
                        return Outcome::Stalled(format!(
                            "{node} is still taking an earlier Serve entry for {host} off its \
                             config, and Temps never revives one being removed. Resume once \
                             it has; its agent reports every 30 seconds."
                        ));
                    }
                    Err(error) => {
                        self.finish_failed(&intent, true).await;
                        return Outcome::Failed(error);
                    }
                }
            }
        }
        let mut waited = Duration::ZERO;
        let mut last = format!(
            "{node} has not applied the Serve entry for {host} yet; its agent applies it on its \
             next pass, every 30 seconds."
        );
        loop {
            match self.ops.serve_entry(target.node_id, &address_id).await {
                Ok(None) => {
                    return Outcome::Failed(format!(
                        "The Serve entry for {host} on {node} is gone: something removed it \
                         while this run waited."
                    ))
                }
                Ok(Some(status)) if status.removing => {
                    return Outcome::Failed(format!(
                        "The Serve entry for {host} on {node} is being removed: another run's \
                         undo took it off while this run waited."
                    ))
                }
                Ok(Some(status)) if status.funnel => {
                    return Outcome::Conflict(tailnet_messages::funnel(&node))
                }
                Ok(Some(status)) => match status.report.as_deref() {
                    Some(REPORT_APPLIED) => {
                        return Outcome::Done(Some(format!(
                            "{node} serves {host} with Tailscale Serve."
                        )))
                    }
                    Some(REPORT_CONFLICT) => {
                        return Outcome::Conflict(tailnet_messages::slot_taken(
                            &host,
                            status
                                .detail
                                .as_deref()
                                .unwrap_or("a Serve entry Temps did not write"),
                        ))
                    }
                    Some(REPORT_ERROR) => {
                        last = format!(
                            "{node} could not apply the Serve entry for {host}: {}",
                            status.detail.unwrap_or_default()
                        )
                    }
                    _ => {
                        if let Some(error) = status.worker_error {
                            last = error;
                        }
                    }
                },
                Err(error) => last = format!("Could not read the Serve entry for {host}: {error}"),
            }
            if waited >= self.timing.publish {
                return Outcome::Stalled(last);
            }
            self.ops.sleep(self.timing.poll).await;
            waited += self.timing.poll.max(Duration::from_millis(1));
        }
    }

    /// The Tailscale Service this run defined, as journaled.
    fn service_resource(&self) -> Option<RunResource> {
        let name = &self.input.tailscale_service.as_ref()?.name;
        self.record.journal.iter().find_map(|e| match &e.resource {
            Some(r @ RunResource::TailscaleService { name: n, .. })
                if n == name && e.state == JournalState::Done =>
            {
                Some(r.clone())
            }
            _ => None,
        })
    }

    /// Record `resource` as done under `intent`'s key without a call: what
    /// this run found rather than made.
    async fn record_found(&mut self, intent: &Intent, resource: RunResource) {
        let key = intent.key(self.input.run_id);
        let now = chrono::Utc::now();
        let entry = match self.record.journal.iter().position(|e| e.key == key) {
            Some(at) => &mut self.record.journal[at],
            None => {
                self.record
                    .journal
                    .push(JournalEntry::pending(key, intent.clone(), now));
                let last = self.record.journal.len() - 1;
                &mut self.record.journal[last]
            }
        };
        entry.state = JournalState::Done;
        entry.resource = Some(resource);
        entry.pending_since = None;
        entry.at = now;
        self.save().await;
    }

    /// ADR 065 D2: prove a pasted OAuth client's scopes and save it.
    async fn save_tailscale_credential(&mut self) -> Outcome {
        let input = self.input;
        let Some(plan) = &input.tailscale_service else {
            return Outcome::Skipped("No Tailscale Service.".into());
        };
        let intent = Intent::TailscaleCredential {
            name: plan.credential_name.clone(),
        };
        let key = intent.key(input.run_id);
        let resource = match self.begin(&intent).await {
            Err(hold) => return hold.into(),
            Ok(Begin::Have(resource)) => *resource,
            Ok(Begin::Go) => {
                let Some(credential) = &input.tailscale_credential else {
                    self.finish(&key, None).await;
                    return Outcome::Failed(
                        "Paste the Tailscale OAuth client again: Temps never stores it until the \
                         run saves it."
                            .into(),
                    );
                };
                match self
                    .ops
                    .save_tailscale_credential(&plan.credential_name, credential)
                    .await
                {
                    Ok(id) => {
                        let resource = RunResource::TailscaleCredential {
                            id,
                            name: plan.credential_name.clone(),
                        };
                        self.finish(&key, Some(resource.clone())).await;
                        resource
                    }
                    Err(error) => {
                        self.finish_failed(&intent, true).await;
                        return Outcome::Failed(error);
                    }
                }
            }
        };
        match resource {
            RunResource::TailscaleCredential { name, .. } => {
                Outcome::Done(Some(format!("Every scope is there; saved as {name}.")))
            }
            _ => Outcome::Failed("The saved Tailscale OAuth client could not be found.".into()),
        }
    }

    /// ADR 065 D4: create the Service only when absent, find this install's
    /// own, and check the whole definition and its hosts back.
    async fn define_service(&mut self) -> Outcome {
        let input = self.input;
        let Some(plan) = input.tailscale_service.clone() else {
            return Outcome::Skipped("No Tailscale Service.".into());
        };
        let name = plan.name.clone();
        let marker = tailscale::marker(&plan.install_id, input.run_id);
        let intent = Intent::TailscaleService {
            tailnet: plan.tailnet.clone(),
            name: name.clone(),
            marker: marker.clone(),
        };
        let key = intent.key(input.run_id);
        let resource = match self.service_resource() {
            Some(resource) => resource,
            None => {
                let pending = self
                    .record
                    .journal
                    .iter()
                    .any(|e| e.key == key && e.state == JournalState::Pending);
                // A pending create is resolved by its lookup, never by a
                // second read that could take it for someone else's.
                let read = if pending {
                    None
                } else {
                    match self.ops.read_service(&name).await {
                        Ok(read) => Some(read),
                        Err(error) => {
                            return Outcome::Failed(format!(
                                "Temps could not read `{name}` from your tailnet, so it created \
                                 nothing: {error}"
                            ))
                        }
                    }
                };
                match read {
                    None | Some(None) => match self.begin(&intent).await {
                        Err(hold) => return hold.into(),
                        Ok(Begin::Have(resource)) => *resource,
                        Ok(Begin::Go) => {
                            let fixed = ServiceDefinition::fixed(&name, &plan.tag, &marker);
                            match self.ops.create_service(&fixed).await {
                                Ok(()) => {
                                    let resource = RunResource::TailscaleService {
                                        tailnet: plan.tailnet.clone(),
                                        name: name.clone(),
                                        created: true,
                                        creator_run_id: input.run_id,
                                        definition: fixed,
                                    };
                                    self.finish(&key, Some(resource.clone())).await;
                                    resource
                                }
                                Err(error) => {
                                    self.finish_failed(&intent, error.maybe_applied).await;
                                    return Outcome::Failed(error.message);
                                }
                            }
                        }
                    },
                    // A resumed run whose earlier PUT landed.
                    Some(Some(found)) if found.comment == marker => {
                        let resource = RunResource::TailscaleService {
                            tailnet: plan.tailnet.clone(),
                            name: name.clone(),
                            created: true,
                            creator_run_id: input.run_id,
                            definition: found,
                        };
                        self.record_found(&intent, resource.clone()).await;
                        resource
                    }
                    Some(Some(found)) => {
                        match tailscale::ownership(&found, &plan.install_id) {
                            Ownership::Ours { creator_run_id } => {
                                match plan.seen.as_ref().map(|seen| found.differs_from(seen)) {
                                    // D1 option 1: this install's, as the plan read it.
                                    Some(None) => {
                                        let resource = RunResource::TailscaleService {
                                            tailnet: plan.tailnet.clone(),
                                            name: name.clone(),
                                            created: false,
                                            creator_run_id,
                                            definition: found,
                                        };
                                        self.record_found(&intent, resource.clone()).await;
                                        resource
                                    }
                                    Some(Some(why)) => {
                                        return Outcome::Conflict(format!(
                                            "{why} since this run was planned. Temps wrote \
                                             nothing to it: check it, then plan again."
                                        ))
                                    }
                                    None => {
                                        return Outcome::Conflict(format!(
                                            "`{name}` was created by another setup of this Temps \
                                             install (run {creator_run_id}) after this run was \
                                             planned. Temps wrote nothing to it: plan again to \
                                             use it."
                                        ))
                                    }
                                }
                            }
                            Ownership::Foreign => {
                                return Outcome::Conflict(tailnet_messages::service_taken(&name))
                            }
                        }
                    }
                }
            }
        };
        self.check_service(resource, &plan, &marker).await
    }

    /// When the Service step is done (ADR 065 D4): its marker, the fixed
    /// definition, Tailscale addresses, and only recorded hosts.
    async fn check_service(
        &mut self,
        resource: RunResource,
        plan: &ServicePlan,
        marker: &str,
    ) -> Outcome {
        let RunResource::TailscaleService {
            created,
            creator_run_id,
            ..
        } = resource.clone()
        else {
            return Outcome::Failed("The Tailscale Service could not be recorded.".into());
        };
        let name = plan.name.as_str();
        // Tailscale assigns the TailVIPs: wait for them to show.
        let mut waited = Duration::ZERO;
        let read = loop {
            match self.ops.read_service(name).await {
                Ok(Some(read)) if !read.addrs.is_empty() || waited >= self.timing.publish => {
                    break read
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Outcome::Failed(format!(
                        "`{name}` is gone from your tailnet: something deleted it while this run \
                         worked on it."
                    ))
                }
                Err(error) if waited >= self.timing.publish => {
                    return Outcome::Stalled(format!(
                        "Could not read `{name}` back from your tailnet: {error}"
                    ))
                }
                Err(_) => {}
            }
            self.ops.sleep(self.timing.poll).await;
            waited += self.timing.poll.max(Duration::from_millis(1));
        };
        let owned = if created {
            (read.comment == marker).then_some(())
        } else {
            matches!(
                tailscale::ownership(&read, &plan.install_id),
                Ownership::Ours { .. }
            )
            .then_some(())
        };
        if owned.is_none() {
            return Outcome::Conflict(format!(
                "`{name}` now carries the comment `{}`, not this Temps install's marker. Temps \
                 writes nothing to it: check who changed it.",
                read.comment
            ));
        }
        if let Some(why) = read.differs_from_fixed(&plan.tag) {
            return Outcome::Conflict(format!("{why}. Temps never changes a Service: check it."));
        }
        if !tailscale_addrs(&read.addrs) {
            return Outcome::Conflict(format!(
                "`{name}` has the addresses {:?}, not Tailscale ones, so Temps does not trust it.",
                read.addrs
            ));
        }
        if !created {
            match self.ops.service_creation(name, creator_run_id).await {
                Ok(Some((journaled, _))) if journaled.addrs.is_empty() => {}
                Ok(Some((journaled, _))) => {
                    if let Some(why) = read.differs_from(&ServiceDefinition {
                        comment: read.comment.clone(),
                        ..journaled
                    }) {
                        return Outcome::Conflict(format!("{why}. Temps never changes a Service."));
                    }
                }
                Ok(None) => {
                    return Outcome::Conflict(format!(
                        "No setup of this Temps install records creating `{name}` (run \
                         {creator_run_id}), so Temps does not use it. Pick another name."
                    ))
                }
                Err(error) => return Outcome::Failed(error),
            }
        }
        if let Err(outcome) = self.unrecorded(name).await {
            return outcome;
        }
        // Journal what was read, with the addresses Tailscale assigned.
        let resource = RunResource::TailscaleService {
            tailnet: plan.tailnet.clone(),
            name: name.to_string(),
            created,
            creator_run_id,
            definition: read.clone(),
        };
        let key = Intent::TailscaleService {
            tailnet: plan.tailnet.clone(),
            name: name.to_string(),
            marker: marker.to_string(),
        }
        .key(self.input.run_id);
        if let Some(entry) = self.record.journal.iter_mut().find(|e| e.key == key) {
            entry.resource = Some(resource);
        }
        self.save().await;
        Outcome::Done(Some(format!(
            "`{name}` is in your tailnet at {}{}.",
            read.addrs.join(", "),
            if created {
                ""
            } else {
                ", as this Temps install made it"
            }
        )))
    }

    /// Every host advertising the Service must match one of this address's
    /// Serve entries for it (ADR 065 D4); `Err` the outcome when one does
    /// not, or the hosts cannot be read.
    async fn unrecorded(&self, name: &str) -> Result<Vec<ServiceHost>, Outcome> {
        let hosts = self.ops.service_hosts(name).await.map_err(|error| {
            Outcome::Failed(format!(
                "Temps could not read which machines host `{name}`, which it checks before \
                 trusting it: {error}"
            ))
        })?;
        let entries = self
            .ops
            .service_entries(name)
            .await
            .map_err(Outcome::Failed)?;
        let address_id = match self.address_id().await {
            Ok(id) => id,
            Err(error) => return Err(Outcome::Failed(error)),
        };
        let foreign = unrecorded_hosts(&hosts, &entries, Some(&address_id));
        if foreign.is_empty() {
            Ok(hosts)
        } else {
            Err(Outcome::Conflict(unrecorded_message(name, &foreign)))
        }
    }

    /// ADR 065 D4: wait for the tailnet admin to approve the worker as the
    /// Service's host. It never approves; waiting is not a failure.
    async fn wait_for_approval(&mut self) -> Outcome {
        let input = self.input;
        let (Some(plan), Some(target)) = (&input.tailscale_service, &input.serve) else {
            return Outcome::Skipped("No Tailscale Service.".into());
        };
        let name = plan.name.as_str();
        let node = target.node_name.as_str();
        let mut waited = Duration::ZERO;
        let mut last;
        loop {
            // Deleted after the define step: no one recreates it behind
            // the user's back, so the run stops and undo takes it apart.
            if let Ok(None) = self.ops.read_service(name).await {
                return Outcome::Failed(format!(
                    "`{name}` is gone from your tailnet: something deleted it after this run \
                     defined it. Undo, then make the address reachable again."
                ));
            }
            let hosts = match self.unrecorded(name).await {
                Ok(hosts) => hosts,
                Err(outcome) => return outcome,
            };
            let mine = match self.ops.service_entries(name).await {
                Ok(entries) => entries.into_iter().find(|e| e.node_id == target.node_id),
                Err(error) => return Outcome::Failed(error),
            };
            let host = mine
                .as_ref()
                .and_then(|e| hosts.iter().find(|h| e.hosts(h)));
            match host.map(|h| h.approved) {
                Some(Some(true)) => {
                    return Outcome::Done(Some(format!("{node} is the approved host of `{name}`.")))
                }
                Some(_) => last = tailnet_messages::not_approved(name, node, &plan.tag),
                None => {
                    last = format!(
                        "`{node}` does not advertise `{name}` yet; its agent does on its next \
                         pass, every 30 seconds. Then {}",
                        tailnet_messages::not_approved(name, node, &plan.tag)
                    )
                }
            }
            if waited >= self.timing.publish {
                return Outcome::Stalled(last);
            }
            self.ops.sleep(self.timing.poll).await;
            waited += self.timing.poll.max(Duration::from_millis(1));
        }
    }

    async fn step(&mut self, index: usize, kind: StepKind) -> Outcome {
        let input = self.input;
        let address = input.address.clone();
        match kind {
            StepKind::CheckToken => {
                let (Some(token), Some(new)) = (&input.token, &input.new_tunnel) else {
                    return Outcome::Failed(
                        "Paste the Cloudflare API token again: Temps never stores it until \
                         the run saves it."
                            .into(),
                    );
                };
                let result = self.ops.check_token(token, &new.zone).await;
                match result.refusal() {
                    Some(reason) => Outcome::Failed(reason),
                    None => Outcome::Done(Some(
                        "Every permission is there, for this zone only.".into(),
                    )),
                }
            }
            StepKind::SaveDnsProvider => {
                let Some(new) = &input.new_tunnel else {
                    return Outcome::Skipped("No new tunnel.".into());
                };
                let intent = Intent::DnsProvider {
                    name: new.provider_name.clone(),
                };
                let resource = match self.begin(&intent).await {
                    Err(hold) => return hold.into(),
                    Ok(Begin::Have(resource)) => *resource,
                    Ok(Begin::Go) => {
                        let Some(token) = &input.token else {
                            self.finish(&intent.key(input.run_id), None).await;
                            return Outcome::Failed(
                                "Paste the Cloudflare API token again: Temps never stores it \
                                 until the run saves it."
                                    .into(),
                            );
                        };
                        match self
                            .ops
                            .save_dns_provider(&new.provider_name, &new.zone, token)
                            .await
                        {
                            Ok(id) => {
                                let resource = RunResource::DnsProvider {
                                    id,
                                    name: new.provider_name.clone(),
                                };
                                self.finish(&intent.key(input.run_id), Some(resource.clone()))
                                    .await;
                                resource
                            }
                            Err(error) => {
                                self.finish_failed(&intent, true).await;
                                return Outcome::Failed(error);
                            }
                        }
                    }
                };
                if let RunResource::DnsProvider { id, name } = resource {
                    self.ctx.provider_id = Some(id);
                    Outcome::Done(Some(format!("Added as {name}.")))
                } else {
                    Outcome::Failed("The saved Cloudflare account could not be found.".into())
                }
            }
            StepKind::ManageDomain => {
                let Some(new) = &input.new_tunnel else {
                    return Outcome::Skipped("No new tunnel.".into());
                };
                let Some(provider_id) = self.ctx.provider_id else {
                    return Outcome::Failed(
                        "No Cloudflare account to manage the domain with.".into(),
                    );
                };
                let zone = new.zone.clone();
                let add = Intent::ManagedDomain {
                    provider_id,
                    domain: zone.clone(),
                };
                let enable = Intent::AutoManageEnabled {
                    provider_id,
                    domain: zone.clone(),
                };
                let state = if self.known(&add) || self.known(&enable) {
                    None
                } else {
                    match self.ops.managed_zone(provider_id, &zone).await {
                        Ok(state) => state,
                        Err(error) => return Outcome::Failed(error),
                    }
                };
                let mut verified = state.is_some_and(|s| s.verified);
                let found = state.is_some();
                match state {
                    None if !self.known(&enable) => {
                        self.set(
                            index,
                            RunStepStatus::Running,
                            Some(format!("Adding {zone} to Temps…")),
                        )
                        .await;
                        match self.begin(&add).await {
                            Err(hold) => return hold.into(),
                            Ok(Begin::Have(_)) => {
                                verified = self
                                    .ops
                                    .managed_zone(provider_id, &zone)
                                    .await
                                    .ok()
                                    .flatten()
                                    .is_some_and(|s| s.verified);
                            }
                            Ok(Begin::Go) => {
                                if let Err(error) =
                                    self.ops.add_managed_zone(provider_id, &zone).await
                                {
                                    self.finish_failed(&add, true).await;
                                    return Outcome::Failed(error);
                                }
                                self.finish(
                                    &add.key(input.run_id),
                                    Some(RunResource::ManagedDomain {
                                        provider_id,
                                        domain: zone.clone(),
                                    }),
                                )
                                .await;
                            }
                        }
                    }
                    Some(ZoneState {
                        auto_manage: true, ..
                    }) => {}
                    _ => match self.begin(&enable).await {
                        Err(hold) => return hold.into(),
                        Ok(Begin::Have(_)) => {}
                        Ok(Begin::Go) => {
                            if let Err(error) =
                                self.ops.set_auto_manage(provider_id, &zone, true).await
                            {
                                self.finish_failed(&enable, true).await;
                                return Outcome::Failed(error);
                            }
                            self.finish(
                                &enable.key(input.run_id),
                                Some(RunResource::AutoManageEnabled {
                                    provider_id,
                                    domain: zone.clone(),
                                }),
                            )
                            .await;
                        }
                    },
                }
                if !verified {
                    self.set(
                        index,
                        RunStepStatus::Running,
                        Some("Checking the token can manage it…".into()),
                    )
                    .await;
                    match self.ops.verify_zone(provider_id, &zone).await {
                        Ok(true) => {}
                        Ok(false) => {
                            return Outcome::Failed(format!(
                                "Cloudflare did not confirm the token can manage {zone}. Add \
                                 the zone to the token's Zone Resources (or, if your domain's \
                                 zone in Cloudflare is not {zone}, use a hostname right under \
                                 it), then try again."
                            ))
                        }
                        Err(error) => return Outcome::Failed(error),
                    }
                }
                Outcome::Done(Some(if found {
                    "Already in Temps.".into()
                } else {
                    "Added and verified.".into()
                }))
            }
            StepKind::CreateTunnel => {
                let Some(new) = input.new_tunnel.clone() else {
                    return Outcome::Skipped("No new tunnel.".into());
                };
                let Some(provider_id) = self.ctx.provider_id else {
                    return Outcome::Failed(
                        "No Cloudflare account to create the tunnel with.".into(),
                    );
                };
                let intent = Intent::Connector {
                    name: new.name.clone(),
                };
                match self.begin(&intent).await {
                    Err(hold) => hold.into(),
                    Ok(Begin::Have(resource)) => {
                        if let RunResource::Connector { id, .. } = *resource {
                            self.ctx.connector_id = Some(id);
                        }
                        Outcome::Done(Some(format!(
                            "Tunnel {} for {} (created earlier).",
                            new.name, new.domain
                        )))
                    }
                    Ok(Begin::Go) => match self
                        .ops
                        .create_tunnel(&new.name, provider_id, &new.domain, &[new.worker.id])
                        .await
                    {
                        Ok((id, status)) => {
                            self.ctx.connector_id = Some(id);
                            self.finish(
                                &intent.key(input.run_id),
                                Some(RunResource::Connector {
                                    id,
                                    name: new.name.clone(),
                                }),
                            )
                            .await;
                            if status.status == "error" {
                                return Outcome::Failed(
                                    status
                                        .last_error
                                        .unwrap_or_else(|| "Cloudflare refused the tunnel.".into()),
                                );
                            }
                            Outcome::Done(Some(format!("Tunnel {} for {}.", new.name, new.domain)))
                        }
                        Err((saved, error)) => {
                            match saved {
                                Some(id) => {
                                    self.finish(
                                        &intent.key(input.run_id),
                                        Some(RunResource::Connector {
                                            id,
                                            name: new.name.clone(),
                                        }),
                                    )
                                    .await
                                }
                                None => self.finish_failed(&intent, true).await,
                            }
                            Outcome::Failed(error)
                        }
                    },
                }
            }
            StepKind::WaitForTunnel => {
                let (Some(id), Some(new)) = (self.ctx.connector_id, &input.new_tunnel) else {
                    return Outcome::Skipped("No new tunnel.".into());
                };
                let worker = new.worker.clone();
                let mut waited = Duration::ZERO;
                loop {
                    match self.ops.tunnel_status(id).await {
                        Ok(status) if status.status == "error" => {
                            return Outcome::Failed(
                                status
                                    .last_error
                                    .unwrap_or_else(|| "Cloudflare refused the tunnel.".into()),
                            )
                        }
                        Ok(status) => {
                            if let Some((_, state, links, error)) =
                                status.replicas.iter().find(|r| r.0 == worker.id)
                            {
                                match state.as_str() {
                                    "ready" => {
                                        return Outcome::Done(Some(match links {
                                            Some(n) => format!(
                                                "{} has {n} connection{} to Cloudflare.",
                                                worker.name,
                                                if *n == 1 { "" } else { "s" }
                                            ),
                                            None => format!("{} is connected.", worker.name),
                                        }))
                                    }
                                    "refused" => {
                                        return Outcome::Failed(format!(
                                            "{} could not run the tunnel{}",
                                            worker.name,
                                            error
                                                .as_ref()
                                                .map(|e| format!(": {e}"))
                                                .unwrap_or_else(|| ".".into())
                                        ))
                                    }
                                    _ => {
                                        self.set(
                                            index,
                                            RunStepStatus::Running,
                                            Some(match error {
                                                Some(e) => format!("{state}: {e}"),
                                                None => format!("{}: {state}", worker.name),
                                            }),
                                        )
                                        .await
                                    }
                                }
                            }
                        }
                        // A missed poll is not a failure; the deadline still applies.
                        Err(error) => {
                            self.set(
                                index,
                                RunStepStatus::Running,
                                Some(format!("Status unavailable: {error}")),
                            )
                            .await
                        }
                    }
                    if waited >= self.timing.connect {
                        // Slow is not broken: the rest can be set up meanwhile.
                        return Outcome::Done(Some(format!(
                            "{} has not connected yet; it keeps trying.",
                            worker.name
                        )));
                    }
                    self.ops.sleep(self.timing.poll).await;
                    waited += self.timing.poll.max(Duration::from_millis(1));
                }
            }
            StepKind::AttachDomain => {
                let intent = Intent::CustomDomain {
                    domain: address.clone(),
                    environment_id: input.environment_id,
                };
                if !self.known(&intent) {
                    match self.ops.find_custom_domain(&address).await {
                        Ok(Some((_, environment_id))) if environment_id == input.environment_id => {
                            return Outcome::Skipped(format!(
                                "Already on {}.",
                                input.environment_name
                            ))
                        }
                        Ok(Some(_)) => {
                            return Outcome::Failed(format!(
                                "{address} is attached to another environment. Remove it there \
                                 first, or use another hostname."
                            ))
                        }
                        Ok(None) => {}
                        Err(error) => return Outcome::Failed(error),
                    }
                }
                match self.begin(&intent).await {
                    Err(hold) => hold.into(),
                    Ok(Begin::Have(_)) => Outcome::Done(None),
                    Ok(Begin::Go) => match self
                        .ops
                        .attach_domain(input.project_id, input.environment_id, &address)
                        .await
                    {
                        Ok(id) => {
                            self.finish(
                                &intent.key(input.run_id),
                                Some(RunResource::CustomDomain {
                                    id,
                                    domain: address.clone(),
                                }),
                            )
                            .await;
                            Outcome::Done(None)
                        }
                        Err(error) => {
                            self.finish_failed(&intent, true).await;
                            Outcome::Failed(error)
                        }
                    },
                }
            }
            StepKind::RouteHostname => {
                let Some(connector_id) = self
                    .ctx
                    .connector_id
                    .or(input.tunnel.as_ref().map(|t| t.id))
                else {
                    return Outcome::Skipped("No tunnel to publish through.".into());
                };
                let known = self
                    .record
                    .journal
                    .iter()
                    .find(|e| {
                        e.state != JournalState::Absent
                            && matches!(&e.intent, Intent::HostnameChoice { hostname, .. } if *hostname == address)
                    })
                    .map(|e| e.intent.clone());
                let intent = match known {
                    Some(intent) => intent,
                    None => {
                        let current = match self.ops.hostname_choice(&address).await {
                            Ok(current) => current,
                            Err(error) => return Outcome::Failed(error),
                        };
                        if current == Some(Some(connector_id)) {
                            return Outcome::Skipped("Already chosen for this tunnel.".into());
                        }
                        // Another user's choice is changed only when the plan
                        // said so and it was confirmed.
                        if current.is_some() && !input.route_confirmed {
                            return Outcome::Failed(format!(
                                "{address} has a tunnel choice that this setup would change, and \
                                 that was not confirmed. Plan again and confirm the change, or \
                                 change it under Expose your apps."
                            ));
                        }
                        Intent::HostnameChoice {
                            hostname: address.clone(),
                            connector_id,
                            previous: PreviousChoice::from_choice(current),
                        }
                    }
                };
                let Intent::HostnameChoice { previous, .. } = intent.clone() else {
                    return Outcome::Failed("The hostname choice could not be recorded.".into());
                };
                match self.begin(&intent).await {
                    Err(hold) => hold.into(),
                    Ok(Begin::Have(_)) => Outcome::Done(None),
                    Ok(Begin::Go) => match self.ops.route_hostname(&address, connector_id).await {
                        Ok(()) => {
                            self.finish(
                                &intent.key(input.run_id),
                                Some(RunResource::HostnameChoice {
                                    hostname: address.clone(),
                                    connector_id,
                                    previous,
                                }),
                            )
                            .await;
                            Outcome::Done(None)
                        }
                        Err(error) => {
                            self.finish_failed(&intent, true).await;
                            Outcome::Failed(error)
                        }
                    },
                }
            }
            StepKind::SetDnsRecord => {
                let Some((_, ip)) = input.direct.clone() else {
                    return Outcome::Failed("No address to point the record at.".into());
                };
                self.set_dns_record(&address, ip).await
            }
            StepKind::ShowDnsRecord => {
                let Some((_, ip)) = input.direct.clone() else {
                    return Outcome::Failed("No address to point the record at.".into());
                };
                let wanted = format!(
                    "Add an A record for {address} pointing at {ip} where your domain's DNS is \
                     hosted"
                );
                self.set(
                    index,
                    RunStepStatus::Running,
                    Some(format!("{wanted}; waiting for it…")),
                )
                .await;
                let mut waited = Duration::ZERO;
                loop {
                    if self.ops.resolve(&address).await.contains(&ip) {
                        return Outcome::Done(Some(format!("{address} points at {ip}.")));
                    }
                    if waited >= self.timing.dns {
                        return Outcome::Stalled(format!(
                            "{wanted}, then resume this setup: it picks up where it stopped."
                        ));
                    }
                    self.ops.sleep(self.timing.poll).await;
                    waited += self.timing.poll.max(Duration::from_millis(1));
                }
            }
            StepKind::IssueCertificate => {
                let intent = Intent::Certificate {
                    domain: address.clone(),
                };
                if !self.known(&intent) {
                    match self.ops.certificate_exists(&address).await {
                        Ok(true) => {
                            return Outcome::Skipped("A certificate already covers it.".into())
                        }
                        Ok(false) => {}
                        Err(error) => return Outcome::Failed(error),
                    }
                }
                let Some(email) = input.acme_email.clone() else {
                    return Outcome::Failed(
                        "Let's Encrypt needs an email address, and your account has none. Add \
                         one to your profile, then try again."
                            .into(),
                    );
                };
                let challenge = input.certificate_challenge.as_deref().unwrap_or("http-01");
                match self.begin(&intent).await {
                    Err(hold) => return hold.into(),
                    Ok(Begin::Have(_)) => {}
                    Ok(Begin::Go) => {
                        if let Err(error) = self.ops.start_certificate(&address, challenge).await {
                            self.finish_failed(&intent, true).await;
                            return Outcome::Failed(error);
                        }
                        self.finish(
                            &intent.key(input.run_id),
                            Some(RunResource::Certificate {
                                domain: address.clone(),
                            }),
                        )
                        .await;
                    }
                }
                self.set(
                    index,
                    RunStepStatus::Running,
                    Some(if challenge == "dns-01" {
                        "Let's Encrypt is checking a DNS record Temps publishes for it…".into()
                    } else {
                        "Let's Encrypt is checking the domain over HTTP…".into()
                    }),
                )
                .await;
                match self.ops.finish_certificate(&address, &email).await {
                    Ok(()) => Outcome::Done(Some("Issued by Let's Encrypt.".into())),
                    Err(error) => Outcome::Failed(error),
                }
            }
            StepKind::WaitForPublication => {
                let mut waited = Duration::ZERO;
                loop {
                    match self.ops.hostname_state(&address).await {
                        Ok((state, detail)) if state == "published" => {
                            return Outcome::Done(Some(detail))
                        }
                        Ok((state, detail)) if state == "error" => return Outcome::Stalled(detail),
                        Ok((_, detail)) => {
                            if waited >= self.timing.publish {
                                return Outcome::Stalled(format!(
                                    "Not published yet: {detail} Temps keeps trying every \
                                     minute."
                                ));
                            }
                            self.set(index, RunStepStatus::Running, Some(detail)).await;
                        }
                        Err(error) => {
                            if waited >= self.timing.publish {
                                return Outcome::Stalled(format!(
                                    "Could not tell whether it is published: {error}"
                                ));
                            }
                        }
                    }
                    self.ops.sleep(self.timing.poll).await;
                    waited += self.timing.poll.max(Duration::from_millis(1));
                }
            }
            StepKind::SetAudience => {
                let Some((after, before)) = input.audience.clone() else {
                    return Outcome::Skipped("Nothing to change.".into());
                };
                let intent = Intent::AddressAudience {
                    hostname: address.clone(),
                    before,
                    after: after.clone(),
                };
                if !self.known(&intent) {
                    match self.ops.address_audience(&address).await {
                        Ok(Some(current)) if current == after => {
                            return Outcome::Skipped(format!(
                                "{address} is already {}.",
                                after.describe()
                            ))
                        }
                        Ok(_) => {}
                        Err(error) => return Outcome::Failed(error),
                    }
                }
                match self.begin(&intent).await {
                    Err(hold) => hold.into(),
                    Ok(Begin::Have(_)) => {
                        Outcome::Done(Some(format!("{address} is {}.", after.describe())))
                    }
                    Ok(Begin::Go) => match self.ops.set_address_audience(&address, &after).await {
                        Ok(()) => {
                            let Intent::AddressAudience { before, .. } = intent.clone() else {
                                return Outcome::Failed(
                                    "The audience could not be recorded.".into(),
                                );
                            };
                            self.finish(
                                &intent.key(input.run_id),
                                Some(RunResource::AddressAudience {
                                    hostname: address.clone(),
                                    before,
                                    after: after.clone(),
                                }),
                            )
                            .await;
                            Outcome::Done(Some(format!("{address} is {}.", after.describe())))
                        }
                        Err(error) => {
                            self.finish_failed(&intent, true).await;
                            Outcome::Failed(error)
                        }
                    },
                }
            }
            StepKind::ConfigureTailscaleServe => {
                let Some(target) = input.serve.clone() else {
                    return Outcome::Skipped("No worker serves it through Tailscale.".into());
                };
                self.configure_serve(&target).await
            }
            StepKind::SaveTailscaleCredential => self.save_tailscale_credential().await,
            StepKind::DefineTailscaleService => self.define_service().await,
            StepKind::WaitForServiceApproval => self.wait_for_approval().await,
            StepKind::CheckReachability => {
                let verdict = self.ops.reachability(&address).await;
                if verdict.state == ReachState::Reachable {
                    self.record.live_url = Some(format!("https://{address}"));
                    Outcome::Done(Some(verdict.reason))
                } else {
                    Outcome::Stalled(verdict.reason)
                }
            }
        }
    }

    async fn drive(mut self, from: usize) -> RunRecord {
        self.save().await;
        for index in from..self.input.steps.len() {
            let kind = self.input.steps[index].kind;
            self.set(index, RunStepStatus::Running, None).await;
            match self.step(index, kind).await {
                Outcome::Done(detail) => self.set(index, RunStepStatus::Done, detail).await,
                Outcome::Skipped(detail) => {
                    self.set(index, RunStepStatus::Skipped, Some(detail)).await
                }
                Outcome::Stalled(reason) => {
                    self.set(index, RunStepStatus::Failed, Some(reason.clone()))
                        .await;
                    self.record.state = RunState::Waiting;
                    self.record.error = Some(reason);
                    self.save().await;
                    return self.record;
                }
                Outcome::Conflict(reason) => {
                    // Everything is kept: undoing now could put back what
                    // someone else just changed.
                    self.set(index, RunStepStatus::Failed, Some(reason.clone()))
                        .await;
                    self.record.state = RunState::Conflict;
                    self.record.error = Some(reason);
                    self.save().await;
                    return self.record;
                }
                Outcome::Failed(error) => {
                    self.set(index, RunStepStatus::Failed, Some(error.clone()))
                        .await;
                    self.record.state = RunState::Undoing;
                    self.save().await;
                    let had = !self.record.created().is_empty() || self.record.has_pending();
                    let journal_sink = RecordJournal {
                        sink: self.sink,
                        record: tokio::sync::Mutex::new(self.record.clone()),
                    };
                    let outcome = undo(
                        &mut self.record.journal,
                        self.ops,
                        self.input.run_id,
                        &journal_sink,
                    )
                    .await;
                    let failed: Vec<String> = outcome
                        .results
                        .iter()
                        .filter(|r| !r.ok)
                        .map(|r| r.label.clone())
                        .collect();
                    self.record.undo = outcome.results;
                    self.record.state = RunState::Failed;
                    self.record.error = Some(match outcome.stopped {
                        Some(Stop::Conflict(why)) => {
                            self.record.state = RunState::UndoPaused;
                            format!("{error} Undoing it stopped: {why}")
                        }
                        Some(Stop::Pending(why)) => {
                            // The reconciler resolves it; undo after that.
                            self.record.state = RunState::Recovering;
                            format!("{error} Undoing it stopped: {why}")
                        }
                        Some(Stop::Failed(why)) => format!("{error} Undoing it stopped: {why}"),
                        None if !had => error,
                        None if failed.is_empty() => {
                            format!("{error} What this run had created was removed.")
                        }
                        None => format!(
                            "{error} Could not remove everything this run created: {}.",
                            failed.join("; ")
                        ),
                    });
                    self.save().await;
                    return self.record;
                }
            }
        }
        self.record.state = if self.record.live_url.is_some() {
            RunState::Succeeded
        } else {
            RunState::Waiting
        };
        self.save().await;
        self.record
    }
}

/// The context earlier steps left, from what the journal says they made.
fn context(input: &RunInput, journal: &[JournalEntry]) -> Context {
    let mut ctx = Context {
        provider_id: input.new_tunnel.as_ref().and_then(|t| t.provider_id),
        connector_id: None,
    };
    for resource in created(journal) {
        match resource {
            RunResource::DnsProvider { id, .. } => ctx.provider_id = Some(id),
            RunResource::Connector { id, .. } => ctx.connector_id = Some(id),
            _ => {}
        }
    }
    ctx
}

/// Carry out a run. The record is saved after every change and returned
/// finished.
pub async fn execute(
    input: &RunInput,
    ops: &dyn RunOps,
    sink: &dyn RunSink,
    timing: Timing,
) -> RunRecord {
    Runner {
        input,
        ops,
        sink,
        timing,
        record: RunRecord::new(&input.steps),
        ctx: context(input, &[]),
    }
    .drive(0)
    .await
}

/// Carry on a run that stopped (waiting, or interrupted by a restart) from
/// its first step not done. Its journal must have no pending entry left:
/// the reconciler resolves those first.
pub async fn resume(
    input: &RunInput,
    mut record: RunRecord,
    ops: &dyn RunOps,
    sink: &dyn RunSink,
    timing: Timing,
) -> RunRecord {
    let from = record
        .steps
        .iter()
        .position(|s| !matches!(s.status, RunStepStatus::Done | RunStepStatus::Skipped))
        .unwrap_or(record.steps.len());
    for step in record.steps.iter_mut().skip(from) {
        step.status = RunStepStatus::Pending;
        step.detail = None;
    }
    record.state = RunState::Running;
    record.error = None;
    record.live_url = None;
    let ctx = context(input, &record.journal);
    Runner {
        input,
        ops,
        sink,
        timing,
        record,
        ctx,
    }
    .drive(from)
    .await
}

/// Why undo stopped before the oldest entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// An entry whose outcome Temps cannot tell yet.
    Pending(String),
    /// A record someone else changed after this run wrote it.
    Conflict(String),
    /// A check undo needed could not be made.
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoOutcome {
    pub results: Vec<UndoResult>,
    /// Set when undo stopped: nothing older than that entry was touched.
    pub stopped: Option<Stop>,
}

fn undo_conflict_message(entry: &JournalEntry) -> String {
    match &entry.intent {
        Intent::Connector { name } => format!(
            "Tunnel {name} now also publishes {}, which this run did not set up (sync published \
             them through the tunnel's automatic publishing). Removing the tunnel takes them \
             off the internet. Temps did not touch it, nor anything this run did before it. \
             Keep the tunnel (undo leaves it and the Cloudflare account it uses), or remove it \
             anyway.",
            entry.conflict.as_deref().unwrap_or("other hostnames")
        ),
        Intent::DnsRecord { .. } => format!(
            "{} now holds {}, which this run did not write. Temps never deletes a record \
             without this run's marker, so it did not touch it, nor anything this run did \
             before it. Keep it to carry on with the rest.",
            entry.intent.describe(),
            entry.conflict.as_deref().unwrap_or("something else")
        ),
        Intent::TailscaleServe {
            node_name,
            hostname,
            ..
        } => format!(
            "Temps did not take {hostname} off {node_name}: {}. It never removes Serve \
             configuration it did not write, so it touched nothing there, nor anything this run \
             did before it. Keep it to forget Temps' Serve entry and leave {node_name} as it is, \
             then undo carries on.",
            entry
                .conflict
                .as_deref()
                .unwrap_or("its Serve config holds something else")
        ),
        // The question itself: delete it, or what keeps it (ADR 065 D4).
        Intent::TailscaleService { name, .. } => entry
            .conflict
            .clone()
            .unwrap_or_else(|| format!("Temps stopped at `{name}`. Keep it, or delete it.")),
        _ => format!(
            "{} was changed by someone else after this run set it: it is now {}. Temps did not \
             touch it, nor anything this run did before it. Keep that value, or restore what \
             was there before this run.",
            entry.intent.describe(),
            entry.conflict.as_deref().unwrap_or("something else")
        ),
    }
}

/// Every hostname a journal's steps name: what the run itself set up.
fn journaled_hostnames(journal: &[JournalEntry]) -> std::collections::BTreeSet<String> {
    journal
        .iter()
        .filter_map(|e| match &e.intent {
            Intent::CustomDomain { domain, .. } => Some(domain.clone()),
            Intent::HostnameChoice { hostname, .. }
            | Intent::DnsRecord { hostname, .. }
            | Intent::DnsChange { hostname, .. }
            | Intent::AddressAudience { hostname, .. } => Some(hostname.clone()),
            Intent::Certificate { domain } => Some(domain.clone()),
            Intent::TailscaleServe { hostname, .. } => Some(hostname.clone()),
            _ => None,
        })
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .collect()
}

/// Mark the entry at `index` in conflict with `current` and stop there.
fn pause(entry: &mut JournalEntry, current: String, now: temps_core::DBDateTime) -> Stop {
    entry.state = JournalState::Conflict;
    entry.unwritten = false;
    entry.conflict = Some(current);
    entry.at = now;
    Stop::Conflict(undo_conflict_message(entry))
}

/// Whether `current` still carries the marker run `creator_run_id` of
/// `install` gave it (ADR 065 D4): no marker, another install's or another
/// run's is never written to or deleted. Why not, when it is not.
pub(crate) fn still_ours(
    current: &ServiceDefinition,
    name: &str,
    install: &str,
    creator_run_id: i32,
) -> Result<(), String> {
    let ours = !install.is_empty()
        && tailscale::ownership(current, install) == Ownership::Ours { creator_run_id };
    if ours {
        return Ok(());
    }
    let now = current.comment.trim();
    Err(if now.is_empty() {
        format!("`{name}` no longer carries the marker this Temps install gave it, so it is not Temps' to delete")
    } else {
        format!(
            "`{name}` now carries `{now}`, not the marker this Temps install gave it (`{}`), so it \
             is not Temps' to delete",
            tailscale::marker(install, creator_run_id)
        )
    })
}

/// Where undoing a Tailscale Service left the undo.
enum ServiceUndo {
    /// Carry on, with what to report (boxed: it is much larger than a stop).
    Done(Option<Box<UndoResult>>),
    Stop(Stop),
}

/// Undo a Tailscale Service (ADR 065 D4): keep it while any Serve entry
/// names it, never delete one no journal of this install records as
/// created, pause on a changed definition or an unrecorded host, and
/// otherwise ask the user; delete only once they confirmed what they were
/// shown, with the checks made again under the Service's lock.
async fn undo_service(
    entry: &mut JournalEntry,
    resource: &RunResource,
    ops: &dyn RunOps,
    now: temps_core::DBDateTime,
) -> ServiceUndo {
    let RunResource::TailscaleService {
        tailnet,
        name,
        creator_run_id,
        ..
    } = resource
    else {
        return ServiceUndo::Done(None);
    };
    let kept = |entry: &mut JournalEntry, why: String| {
        entry.state = JournalState::Absent;
        entry.at = now;
        ServiceUndo::Done(Some(Box::new(UndoResult {
            label: format!("Kept `{name}`: {why}"),
            resource: resource.clone(),
            ok: true,
            error: None,
        })))
    };
    let failed = |what: &str, error: String| {
        ServiceUndo::Stop(Stop::Failed(format!(
            "Temps could not {what} `{name}`, so it left it and everything this run did before \
             it: {error} Undo again to retry."
        )))
    };
    // A person confirmed the delete of what they were shown.
    if entry.undo_decision == Some(Decision::Overwrite) {
        let Some(shown) = entry.shown_service.clone() else {
            return kept(entry, "nothing was shown to confirm".to_string());
        };
        return match ops
            .delete_service(tailnet, name, *creator_run_id, &shown)
            .await
        {
            Ok(ServiceDeletion::Deleted) => {
                entry.state = JournalState::Absent;
                entry.at = now;
                ServiceUndo::Done(Some(Box::new(UndoResult {
                    label: format!("Deleted `{name}` from {tailnet}"),
                    resource: resource.clone(),
                    ok: true,
                    error: None,
                })))
            }
            Ok(ServiceDeletion::Gone) => {
                entry.state = JournalState::Absent;
                entry.at = now;
                ServiceUndo::Done(None)
            }
            Ok(ServiceDeletion::Kept(why)) => kept(entry, why),
            Err(error) => failed("delete", error),
        };
    }
    // Still in use: expected, not a conflict.
    let entries = match ops.service_entries(name).await {
        Ok(entries) => entries,
        Err(error) => return failed("tell what uses", error),
    };
    if let Some(user) = entries.first() {
        return kept(
            entry,
            format!("it still serves {} on {}", user.hostname, user.node_name),
        );
    }
    // Never deleted without this install's journal of creating it.
    let creation = match ops.service_creation(name, *creator_run_id).await {
        Ok(Some(creation)) => creation,
        Ok(None) => {
            return kept(
                entry,
                "no setup of this Temps install records creating it".to_string(),
            )
        }
        Err(error) => return failed("find who created", error),
    };
    let current = match ops.read_service(name).await {
        Ok(Some(current)) => current,
        Ok(None) => {
            entry.state = JournalState::Absent;
            entry.at = now;
            return ServiceUndo::Done(None);
        }
        Err(error) => return failed("read", error),
    };
    // A Service that no longer carries the marker this run gave it is not
    // Temps' any more, whatever the journal says (D4): never offered for
    // deletion.
    let owner = tailscale::parse_marker(&creation.0.comment)
        .map(|(install, _)| install)
        .unwrap_or_default();
    if let Err(why) = still_ours(&current, name, &owner, *creator_run_id) {
        return kept(entry, why);
    }
    let changed = if creation.0.addrs.is_empty() {
        current.differs_from(&ServiceDefinition {
            addrs: current.addrs.clone(),
            ..creation.0.clone()
        })
    } else {
        current.differs_from(&creation.0)
    };
    let question = match changed {
        Some(why) => format!(
            "{why}, which Temps did not set. Keep it, or delete it anyway? Temps deletes it only \
             if it still is exactly what you see here."
        ),
        None => {
            let hosts = match ops.service_hosts(name).await {
                Ok(hosts) => hosts,
                Err(error) => return failed("read the hosts of", error),
            };
            let foreign = unrecorded_hosts(&hosts, &[], None);
            if foreign.is_empty() {
                format!(
                    "Delete `{name}` from tailnet `{tailnet}`? It was created by this Temps \
                     install on {} and nothing uses it now.",
                    creation.1.format("%-d %B")
                )
            } else {
                format!(
                    "`{name}` is now also hosted by {}, which no Temps setup configured. Keep \
                     it, or delete it anyway?",
                    foreign
                        .iter()
                        .map(|h| format!("`{h}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    };
    entry.shown_service = Some(current);
    ServiceUndo::Stop(pause(entry, question, now))
}

/// Remove what a run created and put back what it changed, newest first,
/// marking each in the journal. Pre-existing resources are never in it.
///
/// - A created record is removed only while it still holds what this run
///   wrote, with its marker; a changed one goes back only while it still
///   holds exactly this run's *after*. No other DNS record is touched:
///   the records sync published for a tunnel carry sync's marker and go
///   with the connector ([`RunOps::delete_tunnel`]).
/// - A tunnel this run created is checked for hostnames sync published on
///   it that the run never journaled; then marked `deleting`, journaled,
///   and only then removed. One the user chose to keep keeps the DNS
///   provider and zone it depends on too.
/// - Anything Temps cannot account for (still pending, changed by someone
///   else, a tunnel serving more) stops undo there, so nothing older is
///   undone past it. So does a tunnel that could not be removed, since the
///   older steps (its DNS provider) are what removing it needs.
/// - Any other failed removal is reported and the rest still attempted.
pub async fn undo(
    journal: &mut [JournalEntry],
    ops: &dyn RunOps,
    run_id: i32,
    sink: &dyn JournalSink,
) -> UndoOutcome {
    let now = chrono::Utc::now();
    let mut results = Vec::new();
    let stop = |results: Vec<UndoResult>, stop: Stop| UndoOutcome {
        results,
        stopped: Some(stop),
    };
    let journaled = journaled_hostnames(journal);
    let kept_tunnel = journal.iter().find_map(|e| match &e.intent {
        Intent::Connector { name } if e.undo_decision == Some(Decision::Keep) => Some(name.clone()),
        _ => None,
    });
    // Addresses this run attached itself: undo removes them, so their
    // audience is never put back first (a private one would be public in
    // between).
    let attached_here: std::collections::BTreeSet<String> = journal
        .iter()
        .filter(|e| e.state == JournalState::Done)
        .filter_map(|e| match &e.resource {
            Some(RunResource::CustomDomain { domain, .. }) => Some(domain.clone()),
            _ => None,
        })
        .collect();
    for index in (0..journal.len()).rev() {
        let entry = &mut journal[index];
        let resource = match entry.state {
            JournalState::Absent => continue,
            // Temps wrote nothing there.
            JournalState::Conflict if entry.unwritten => continue,
            JournalState::Conflict => {
                return stop(results, Stop::Conflict(undo_conflict_message(entry)))
            }
            JournalState::Pending => match resolve_pending(entry, ops, run_id, now).await {
                Resolution::Done => match entry.resource.clone() {
                    Some(resource) => resource,
                    None => continue,
                },
                Resolution::Absent => continue,
                Resolution::Pending if entry.deleting => {
                    let what = match &entry.intent {
                        Intent::TailscaleServe {
                            node_name,
                            hostname,
                            ..
                        } => format!("{node_name} to take {hostname} off its Serve config"),
                        other => format!("{} to be removed", other.describe()),
                    };
                    return stop(
                        results,
                        Stop::Pending(format!(
                            "Waiting for {what}; undo carries on once it is{}.",
                            entry
                                .conflict
                                .as_deref()
                                .map(|e| format!(" ({e})"))
                                .unwrap_or_default()
                        )),
                    );
                }
                Resolution::Pending => {
                    return stop(
                        results,
                        Stop::Pending(format!(
                            "{} Undo continues once it can tell.",
                            wait_message(entry, now).replace("; resume once it has", "")
                        )),
                    )
                }
                Resolution::Conflict => {
                    return stop(results, Stop::Conflict(undo_conflict_message(entry)))
                }
            },
            JournalState::Done => match entry.resource.clone() {
                Some(resource) => resource,
                None => continue,
            },
        };
        let checked = entry.undo_decision == Some(Decision::Overwrite);
        // ADR 065 D4: this run's reference goes first; the entry, and the
        // worker's configuration, only with the last one.
        if let RunResource::TailscaleServe {
            node_id,
            node_name,
            address_id,
            hostname,
            entry_id,
            ..
        } = &resource
        {
            if entry.deleting && entry.undo_decision == Some(Decision::Keep) {
                let outcome = ops.forget_serve_entry(*entry_id).await;
                if outcome.is_ok() {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                }
                results.push(UndoResult {
                    label: format!(
                        "Left {node_name}'s Serve config as it is and forgot Temps' entry for \
                         {hostname}"
                    ),
                    resource: resource.clone(),
                    ok: outcome.is_ok(),
                    error: outcome.err(),
                });
                continue;
            }
            if entry.deleting {
                // Still waiting on the agent (resolved above when pending).
                return stop(
                    results,
                    Stop::Pending(format!(
                        "Waiting for {node_name} to take {hostname} off its Serve config; \
                         undo carries on once its agent reports it."
                    )),
                );
            }
            match ops.release_serve_entry(run_id, *node_id, address_id).await {
                Ok(ServeRelease::Gone) => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                }
                Ok(ServeRelease::Kept(why)) => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                    results.push(UndoResult {
                        label: format!("Kept {hostname} on {node_name}: {why}"),
                        resource: resource.clone(),
                        ok: true,
                        error: None,
                    });
                }
                Ok(ServeRelease::Deleted) => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                    results.push(UndoResult {
                        label: format!(
                            "Removed Temps' Serve entry for {hostname} ({node_name} had written \
                             nothing for it)"
                        ),
                        resource: resource.clone(),
                        ok: true,
                        error: None,
                    });
                }
                Ok(ServeRelease::Removing) => {
                    entry.deleting = true;
                    entry.state = JournalState::Pending;
                    entry.pending_since = Some(now);
                    entry.next_lookup_at = None;
                    entry.failed_lookups = 0;
                    entry.at = now;
                    if let Err(error) = sink.save_journal(journal).await {
                        return stop(
                            results,
                            Stop::Failed(format!(
                                "{node_name} is taking {hostname} off its Serve config, but \
                                 Temps could not record that: {error} Undo again to carry on."
                            )),
                        );
                    }
                    return stop(
                        results,
                        Stop::Pending(format!(
                            "{node_name} is taking {hostname} off its Serve config; undo \
                             carries on once its agent reports it, within a minute."
                        )),
                    );
                }
                Err(error) => {
                    return stop(
                        results,
                        Stop::Failed(format!(
                            "Temps could not release its Serve entry for {hostname} on \
                             {node_name}, so it touched nothing from there on: {error} Undo \
                             again to retry."
                        )),
                    )
                }
            }
            continue;
        }
        if let RunResource::TailscaleService { .. } = &resource {
            match undo_service(entry, &resource, ops, now).await {
                ServiceUndo::Done(result) => {
                    if let Some(result) = result {
                        results.push(*result);
                    }
                    continue;
                }
                ServiceUndo::Stop(why) => return stop(results, why),
            }
        }
        if let RunResource::TailscaleCredential { id, name } = &resource {
            let outcome = ops.release_tailscale_credential(*id).await;
            let label = match &outcome {
                Ok(Some(why)) => format!("Kept Tailscale OAuth client {name}: {why}"),
                _ => format!("Removed Tailscale OAuth client {name}"),
            };
            if outcome.is_ok() {
                entry.state = JournalState::Absent;
                entry.at = now;
            }
            results.push(UndoResult {
                label,
                resource: resource.clone(),
                ok: outcome.is_ok(),
                error: outcome.err(),
            });
            continue;
        }
        match &resource {
            RunResource::AddressAudience {
                hostname, after, ..
            } if attached_here.contains(hostname) => {
                entry.state = JournalState::Absent;
                entry.at = now;
                results.push(UndoResult {
                    label: format!("Left {hostname} {} until it is removed", after.describe()),
                    resource: resource.clone(),
                    ok: true,
                    error: None,
                });
                continue;
            }
            // What the kept tunnel runs on stays.
            RunResource::DnsProvider { .. }
            | RunResource::ManagedDomain { .. }
            | RunResource::AutoManageEnabled { .. }
                if kept_tunnel.is_some() =>
            {
                entry.state = JournalState::Absent;
                entry.at = now;
                results.push(UndoResult {
                    label: format!(
                        "Kept {}: tunnel {} still uses it",
                        resource.describe(),
                        kept_tunnel.as_deref().unwrap_or_default()
                    ),
                    resource: resource.clone(),
                    ok: true,
                    error: None,
                });
                continue;
            }
            RunResource::DnsRecord {
                hostname,
                record_type,
                content,
                ..
            } => match ops.read_dns_record(hostname, record_type).await {
                Ok(Some(record))
                    if record.managed
                        && record.run_id == Some(run_id)
                        && content.as_ref().is_none_or(|c| *c == record.content) => {}
                // Gone already: nothing to remove.
                Ok(None) => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                    continue;
                }
                Ok(other) => {
                    return stop(results, pause(entry, describe_record(other.as_ref()), now))
                }
                Err(error) => {
                    return stop(
                        results,
                        Stop::Failed(format!(
                            "Temps could not read the {record_type} record for {hostname} to \
                             check it still holds what this run wrote, so it touched nothing \
                             from there on: {error} Undo again to retry."
                        )),
                    )
                }
            },
            RunResource::DnsChange {
                hostname,
                record_type,
                before,
                after,
                ..
            } if !checked => match ops.read_dns_record(hostname, record_type).await {
                Ok(Some(record)) if holds_this_run(&record, after, run_id) => {}
                // An earlier undo put it back already.
                Ok(Some(record)) if record.content == before.content => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                    continue;
                }
                Ok(other) => {
                    return stop(results, pause(entry, describe_record(other.as_ref()), now))
                }
                Err(error) => {
                    return stop(
                        results,
                        Stop::Failed(format!(
                            "Temps could not read the {record_type} record for {hostname} to \
                             check it still holds what this run wrote, so it touched nothing \
                             from there on: {error} Undo again to retry."
                        )),
                    )
                }
            },
            // An audience goes back only while the address, on this run's
            // environment, still holds exactly what the run set: a later
            // change, or the address moving elsewhere, is never reverted.
            RunResource::AddressAudience {
                hostname,
                before,
                after,
            } if !checked => match ops.address_audience(hostname).await {
                Ok(Some(current)) if current == *after => {}
                // An earlier undo put it back already, or it is not here
                // any more: nothing to restore.
                Ok(Some(current)) if current == *before => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                    continue;
                }
                Ok(None) => {
                    entry.state = JournalState::Absent;
                    entry.at = now;
                    continue;
                }
                Ok(Some(current)) => return stop(results, pause(entry, current.describe(), now)),
                Err(error) => {
                    return stop(
                        results,
                        Stop::Failed(format!(
                            "Temps could not read who {hostname} is for to check it still \
                             holds what this run set, so it touched nothing from there on: \
                             {error} Undo again to retry."
                        )),
                    )
                }
            },
            // A tunnel is checked for what else it serves, then marked
            // `deleting`, and that journaled, before any provider call:
            // sync then never brings it back, and an interrupted undo
            // finishes the removal instead of reverting it.
            RunResource::Connector { id, name } if !entry.deleting => {
                if !checked {
                    match ops.tunnel_hostnames(*id).await {
                        Ok(hosts) => {
                            let extra: Vec<String> = hosts
                                .into_iter()
                                .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
                                .filter(|h| !journaled.contains(h))
                                .collect();
                            if !extra.is_empty() {
                                return stop(results, pause(entry, extra.join(", "), now));
                            }
                        }
                        Err(error) => {
                            return stop(
                                results,
                                Stop::Failed(format!(
                                    "Temps could not tell what tunnel {name} publishes now, so \
                                     it left the tunnel and everything this run did before it: \
                                     {error} Undo again to retry."
                                )),
                            )
                        }
                    }
                }
                match ops.mark_tunnel_deleting(*id, name).await {
                    Ok(tunnel_id) => {
                        entry.deleting = true;
                        entry.tunnel_id = tunnel_id;
                        entry.at = now;
                    }
                    Err(error) => {
                        return stop(
                            results,
                            Stop::Failed(format!(
                                "Temps could not start removing tunnel {name}, so it left it and \
                                 everything this run did before it: {error} Undo again to retry."
                            )),
                        )
                    }
                }
                if let Err(error) = sink.save_journal(journal).await {
                    return stop(
                        results,
                        Stop::Failed(format!(
                            "Tunnel {name} is marked for removal, but Temps could not record \
                             that: {error} It finishes removing the tunnel in the background; \
                             undo again to carry on with the rest."
                        )),
                    );
                }
            }
            _ => {}
        }
        let entry = &mut journal[index];
        let outcome = match &resource {
            RunResource::DnsProvider { id, .. } => ops.delete_dns_provider(*id).await,
            RunResource::ManagedDomain {
                provider_id,
                domain,
            } => ops.remove_managed_zone(*provider_id, domain).await,
            RunResource::AutoManageEnabled {
                provider_id,
                domain,
            } => ops.set_auto_manage(*provider_id, domain, false).await,
            RunResource::Connector { id, .. } => {
                ops.delete_tunnel(*id, entry.tunnel_id.as_deref()).await
            }
            RunResource::CustomDomain { id, .. } => ops.detach_domain(*id).await,
            RunResource::HostnameChoice {
                hostname, previous, ..
            } => ops.restore_hostname_choice(hostname, *previous).await,
            RunResource::DnsRecord {
                hostname,
                label,
                record_type,
                ..
            } => ops.remove_dns_record(hostname, label, record_type).await,
            RunResource::DnsChange {
                hostname,
                record_type,
                before,
                ..
            } => ops
                .write_dns_record(hostname, record_type, &before.content, before.run_id)
                .await
                .map(|_| ())
                .map_err(|e| e.message),
            RunResource::AddressAudience {
                hostname, before, ..
            } => ops.set_address_audience(hostname, before).await,
            RunResource::Certificate { domain } => ops.delete_certificate(domain).await,
            // Handled above, before anything is removed.
            RunResource::TailscaleServe { .. }
            | RunResource::TailscaleService { .. }
            | RunResource::TailscaleCredential { .. } => Ok(()),
        };
        let label = match &resource {
            RunResource::AutoManageEnabled { domain, .. } => {
                format!("Turned automatic DNS for {domain} back off")
            }
            RunResource::HostnameChoice {
                hostname,
                previous: PreviousChoice::Off,
                ..
            } => format!("Kept {hostname} off tunnels again"),
            RunResource::HostnameChoice {
                hostname,
                previous: PreviousChoice::Tunnel { .. },
                ..
            } => format!("Put {hostname} back on the tunnel chosen before"),
            RunResource::DnsChange {
                hostname, before, ..
            } => format!("Put {hostname} back to {}", before.describe()),
            RunResource::AddressAudience {
                hostname, before, ..
            } => format!("Made {hostname} {} again", before.describe()),
            other => format!("Removed {}", other.describe()),
        };
        if outcome.is_ok() {
            entry.state = JournalState::Absent;
            entry.at = now;
        } else {
            entry.state = JournalState::Done;
            entry.resource = Some(resource.clone());
        }
        let failed_tunnel = match (&resource, &outcome) {
            (RunResource::Connector { name, .. }, Err(error)) => Some(format!(
                "Tunnel {name} is being removed but is not gone yet: {error} Temps keeps \
                 removing it in the background and never brings it back; undo again to carry \
                 on with what this run did before it."
            )),
            _ => None,
        };
        results.push(UndoResult {
            resource,
            label,
            ok: outcome.is_ok(),
            error: outcome.err(),
        });
        if let Some(why) = failed_tunnel {
            // Its DNS provider, older, is what removing it needs.
            return stop(results, Stop::Failed(why));
        }
    }
    UndoOutcome {
        results,
        stopped: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reachability::model::{AddressSource, Audience};
    use crate::reachability::token_check::{PermissionCheck, PermissionState, TokenPermission};

    fn done(intent: Intent, resource: RunResource) -> JournalEntry {
        JournalEntry {
            state: JournalState::Done,
            resource: Some(resource),
            ..JournalEntry::pending(intent.key(0), intent, chrono::Utc::now())
        }
    }

    /// Journals nowhere, remembering what it was given.
    #[derive(Default)]
    struct Journaled(Mutex<Vec<Vec<JournalEntry>>>);

    #[async_trait]
    impl JournalSink for Journaled {
        async fn save_journal(&self, journal: &[JournalEntry]) -> Result<(), String> {
            self.0.lock().expect("lock").push(journal.to_vec());
            Ok(())
        }
    }

    fn record(content: &str, run_id: Option<i32>, modified: &str) -> DnsRecordState {
        DnsRecordState {
            provider_id: 3,
            label: "myapp".into(),
            record_id: Some("rec-1".into()),
            record_type: "A".into(),
            content: content.into(),
            modified_on: Some(modified.into()),
            managed: true,
            run_id,
        }
    }
    use std::sync::Mutex;

    /// Records every call; fails the ones named in `fail`.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        fail: Vec<&'static str>,
        zone: Option<ZoneState>,
        existing_domain: Option<(i32, i32)>,
        choice: Option<Option<i32>>,
        published_after: usize,
        reachable: bool,
        resolves: bool,
        polls: Mutex<usize>,
        /// What `find` answers, by intent key without the run prefix.
        found: Mutex<Vec<(String, RunResource)>>,
        /// The one DNS record there is.
        dns: Mutex<Option<DnsRecordState>>,
        audience: Mutex<Option<AddressAudience>>,
        /// What the tunnel publishes now.
        served: Mutex<Vec<String>>,
        /// What claiming a Serve entry finds; `None` creates entry 70.
        serve_claim: Option<ServeClaim>,
        /// The Serve entry and its worker's last report.
        serve_status: Mutex<Option<ServeEntryStatus>>,
        /// What releasing this run's reference leaves; `None` is
        /// `Removing`.
        serve_release: Option<ServeRelease>,
        /// This run's reference to the entry.
        serve_ref: Mutex<Option<(i32, bool)>>,
        /// The Service in the tailnet now.
        service: Mutex<Option<ServiceDefinition>>,
        /// The devices advertising it.
        service_hosts: Mutex<Vec<ServiceHost>>,
        /// The Serve entries naming it.
        service_entries: Mutex<Vec<ServiceEntryHost>>,
        /// What `service_creation` answers.
        creation: Option<(ServiceDefinition, temps_core::DBDateTime)>,
        /// What a confirmed delete finds; `None` deletes.
        deletion: Option<ServiceDeletion>,
        /// Reads of the Service so far, and after how many it is gone.
        service_reads: Mutex<usize>,
        vanish_after: Option<usize>,
    }

    impl Fake {
        fn log(&self, call: impl Into<String>) {
            self.calls.lock().expect("lock").push(call.into());
        }
        fn result(&self, name: &'static str) -> Result<(), String> {
            self.log(name);
            if self.fail.contains(&name) {
                Err(format!("{name} broke"))
            } else {
                Ok(())
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock").clone()
        }
        fn exists(&self, intent: &Intent, resource: RunResource) {
            self.found
                .lock()
                .expect("lock")
                .push((intent.key(0), resource));
        }
    }

    #[async_trait]
    impl RunOps for Fake {
        async fn check_token(&self, _token: &str, _zone: &str) -> TokenCheckResult {
            self.log("check_token");
            let state = if self.fail.contains(&"check_token") {
                PermissionState::Missing
            } else {
                PermissionState::Present
            };
            TokenCheckResult {
                valid: true,
                error: None,
                zones: vec!["example.com".into()],
                checked_zone: Some("example.com".into()),
                permissions: vec![PermissionCheck {
                    permission: TokenPermission::DnsEdit,
                    label: "Zone · DNS · Edit".into(),
                    state,
                    detail: String::new(),
                }],
                scope: None,
                ready: state == PermissionState::Present,
                template_url: String::new(),
            }
        }
        async fn find(&self, intent: &Intent) -> Result<Option<RunResource>, String> {
            self.log(format!("find {}", intent.key(0)));
            if self.fail.contains(&"find") {
                return Err("database is down".into());
            }
            let key = intent.key(0);
            Ok(self
                .found
                .lock()
                .expect("lock")
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, r)| r.clone()))
        }
        async fn save_dns_provider(
            &self,
            name: &str,
            _zone: &str,
            _token: &str,
        ) -> Result<i32, String> {
            self.result("save_dns_provider")?;
            assert_eq!(name, "Cloudflare example.com");
            Ok(30)
        }
        async fn delete_dns_provider(&self, id: i32) -> Result<(), String> {
            self.log(format!("delete_dns_provider {id}"));
            Ok(())
        }
        async fn managed_zone(&self, _p: i32, _z: &str) -> Result<Option<ZoneState>, String> {
            self.log("managed_zone");
            Ok(self.zone)
        }
        async fn add_managed_zone(&self, _p: i32, _z: &str) -> Result<(), String> {
            self.result("add_managed_zone")
        }
        async fn set_auto_manage(&self, _p: i32, _z: &str, on: bool) -> Result<(), String> {
            self.log(format!("set_auto_manage {on}"));
            Ok(())
        }
        async fn verify_zone(&self, _p: i32, _z: &str) -> Result<bool, String> {
            self.log("verify_zone");
            Ok(!self.fail.contains(&"verify_zone"))
        }
        async fn remove_managed_zone(&self, _p: i32, zone: &str) -> Result<(), String> {
            self.log(format!("remove_managed_zone {zone}"));
            Ok(())
        }
        async fn create_tunnel(
            &self,
            _name: &str,
            _provider_id: i32,
            _domain: &str,
            _node_ids: &[i32],
        ) -> Result<(i32, TunnelStatus), (Option<i32>, String)> {
            self.log("create_tunnel");
            if self.fail.contains(&"create_tunnel") {
                return Err((Some(40), "Cloudflare refused it".into()));
            }
            Ok((
                40,
                TunnelStatus {
                    status: "ready".into(),
                    last_error: None,
                    replicas: Vec::new(),
                },
            ))
        }
        async fn tunnel_status(&self, _id: i32) -> Result<TunnelStatus, String> {
            self.log("tunnel_status");
            Ok(TunnelStatus {
                status: "ready".into(),
                last_error: None,
                replicas: vec![(2, "ready".into(), Some(4), None)],
            })
        }
        async fn mark_tunnel_deleting(
            &self,
            id: i32,
            _name: &str,
        ) -> Result<Option<String>, String> {
            self.log(format!("mark_tunnel_deleting {id}"));
            Ok(Some(format!("cf-{id}")))
        }
        async fn tunnel_hostnames(&self, id: i32) -> Result<Vec<String>, String> {
            self.log(format!("tunnel_hostnames {id}"));
            Ok(self.served.lock().expect("lock").clone())
        }
        async fn delete_tunnel(&self, id: i32, tunnel_id: Option<&str>) -> Result<(), String> {
            if let Some(tunnel_id) = tunnel_id {
                self.log(format!("tunnel_id {tunnel_id}"));
            }
            self.log(format!("delete_tunnel {id}"));
            if self.fail.contains(&"delete_tunnel") {
                return Err("Cloudflare is down".into());
            }
            Ok(())
        }
        async fn find_custom_domain(&self, _host: &str) -> Result<Option<(i32, i32)>, String> {
            self.log("find_custom_domain");
            Ok(self.existing_domain)
        }
        async fn attach_domain(&self, _p: i32, _e: i32, _host: &str) -> Result<i32, String> {
            self.result("attach_domain")?;
            Ok(50)
        }
        async fn detach_domain(&self, id: i32) -> Result<(), String> {
            self.log(format!("detach_domain {id}"));
            if self.fail.contains(&"detach_domain") {
                return Err("database is down".into());
            }
            Ok(())
        }
        async fn hostname_choice(&self, _host: &str) -> Result<Option<Option<i32>>, String> {
            self.log("hostname_choice");
            Ok(self.choice)
        }
        async fn route_hostname(&self, _host: &str, connector_id: i32) -> Result<(), String> {
            self.log(format!("route_hostname {connector_id}"));
            Ok(())
        }
        async fn restore_hostname_choice(
            &self,
            host: &str,
            previous: PreviousChoice,
        ) -> Result<(), String> {
            self.log(format!("restore_hostname_choice {host} {previous:?}"));
            Ok(())
        }
        async fn hostname_state(&self, _host: &str) -> Result<(String, String), String> {
            let mut polls = self.polls.lock().expect("lock");
            *polls += 1;
            self.log("hostname_state");
            Ok(if *polls > self.published_after {
                (
                    "published".into(),
                    "Published through tunnel 'example-com'.".into(),
                )
            } else {
                ("pending".into(), "Being published.".into())
            })
        }
        async fn read_dns_record(
            &self,
            _host: &str,
            _record_type: &str,
        ) -> Result<Option<DnsRecordState>, String> {
            self.log("read_dns_record");
            if self.fail.contains(&"read_dns_record") {
                return Err("Cloudflare timed out".into());
            }
            Ok(self.dns.lock().expect("lock").clone())
        }
        async fn write_dns_record(
            &self,
            _host: &str,
            _record_type: &str,
            content: &str,
            run_id: Option<i32>,
        ) -> Result<String, WriteError> {
            self.log(format!("write_dns_record {content} {run_id:?}"));
            if self.fail.contains(&"write_dns_record") {
                return Err(WriteError::unknown("the request timed out"));
            }
            if self.fail.contains(&"write_refused") {
                return Err(WriteError::refused("set_dns_record broke"));
            }
            *self.dns.lock().expect("lock") = Some(record(content, run_id, "written"));
            Ok("myapp".into())
        }
        async fn remove_dns_record(
            &self,
            host: &str,
            label: &str,
            record_type: &str,
        ) -> Result<(), String> {
            self.log(format!("remove_dns_record {host} {label} {record_type}"));
            Ok(())
        }
        async fn certificate_exists(&self, _host: &str) -> Result<bool, String> {
            self.log("certificate_exists");
            Ok(false)
        }
        async fn start_certificate(&self, _host: &str, challenge: &str) -> Result<(), String> {
            self.log(format!("challenge {challenge}"));
            self.result("start_certificate")
        }
        async fn finish_certificate(&self, _host: &str, _email: &str) -> Result<(), String> {
            self.result("finish_certificate")
        }
        async fn delete_certificate(&self, host: &str) -> Result<(), String> {
            self.log(format!("delete_certificate {host}"));
            Ok(())
        }
        async fn resolve(&self, _host: &str) -> Vec<IpAddr> {
            self.log("resolve");
            if self.resolves {
                vec!["203.0.113.5".parse().expect("ip")]
            } else {
                Vec::new()
            }
        }
        async fn reachability(&self, host: &str) -> AddressReachability {
            self.log("reachability");
            AddressReachability {
                address: host.into(),
                audience: Audience::Public,
                source: AddressSource::CustomDomain,
                custom_domain_id: None,
                way_in: None,
                state: if self.reachable {
                    ReachState::Reachable
                } else {
                    ReachState::NotReachable
                },
                reason: "checked".into(),
                action: None,
                network: None,
            }
        }
        async fn address_audience(&self, _host: &str) -> Result<Option<AddressAudience>, String> {
            Ok(self.audience.lock().expect("lock").clone())
        }
        async fn set_address_audience(
            &self,
            _host: &str,
            audience: &AddressAudience,
        ) -> Result<(), String> {
            self.log(format!("set_address_audience {}", audience.describe()));
            *self.audience.lock().expect("lock") = Some(audience.clone());
            Ok(())
        }
        async fn claim_serve_entry(
            &self,
            _run_id: i32,
            wish: &ServeWish,
        ) -> Result<ServeClaim, String> {
            self.log(format!(
                "claim_serve_entry {} {} {}",
                wish.target.node_name, wish.address_id, wish.hostname
            ));
            if self.fail.contains(&"claim_serve_entry") {
                return Err("database is down".into());
            }
            let claim = self
                .serve_claim
                .clone()
                .unwrap_or(ServeClaim::Created { entry_id: 70 });
            match &claim {
                ServeClaim::Created { entry_id } => {
                    *self.serve_ref.lock().expect("lock") = Some((*entry_id, true))
                }
                ServeClaim::Found { entry_id } => {
                    *self.serve_ref.lock().expect("lock") = Some((*entry_id, false))
                }
                ServeClaim::Differs(_) | ServeClaim::Removing => {}
            }
            Ok(claim)
        }
        async fn serve_reference(
            &self,
            _run_id: i32,
            _node_id: i32,
            address_id: &str,
        ) -> Result<Option<(i32, bool)>, String> {
            self.log(format!("serve_reference {address_id}"));
            Ok(*self.serve_ref.lock().expect("lock"))
        }
        async fn serve_entry(
            &self,
            _node_id: i32,
            _address_id: &str,
        ) -> Result<Option<ServeEntryStatus>, String> {
            self.log("serve_entry");
            Ok(self.serve_status.lock().expect("lock").clone())
        }
        async fn release_serve_entry(
            &self,
            _run_id: i32,
            _node_id: i32,
            address_id: &str,
        ) -> Result<ServeRelease, String> {
            self.log(format!("release_serve_entry {address_id}"));
            *self.serve_ref.lock().expect("lock") = None;
            let release = self.serve_release.clone().unwrap_or(ServeRelease::Removing);
            if release == ServeRelease::Removing {
                if let Some(status) = self.serve_status.lock().expect("lock").as_mut() {
                    status.removing = true;
                }
            }
            Ok(release)
        }
        async fn forget_serve_entry(&self, entry_id: i32) -> Result<(), String> {
            self.log(format!("forget_serve_entry {entry_id}"));
            Ok(())
        }
        async fn save_tailscale_credential(
            &self,
            name: &str,
            _credential: &TailscaleCredential,
        ) -> Result<i32, String> {
            self.log(format!("save_tailscale_credential {name}"));
            if self.fail.contains(&"save_tailscale_credential") {
                return Err("the OAuth client lacks the `services` scope".into());
            }
            Ok(31)
        }
        async fn release_tailscale_credential(&self, id: i32) -> Result<Option<String>, String> {
            self.log(format!("release_tailscale_credential {id}"));
            Ok(None)
        }
        async fn read_service(&self, name: &str) -> Result<Option<ServiceDefinition>, String> {
            self.log(format!("read_service {name}"));
            if self.fail.contains(&"read_service") {
                return Err("403 Forbidden".into());
            }
            let mut reads = self.service_reads.lock().expect("lock");
            *reads += 1;
            if self.vanish_after.is_some_and(|after| *reads > after) {
                return Ok(None);
            }
            Ok(self.service.lock().expect("lock").clone())
        }
        async fn create_service(&self, definition: &ServiceDefinition) -> Result<(), WriteError> {
            self.log(format!("create_service {}", definition.comment));
            if self.fail.contains(&"create_service") {
                return Err(WriteError {
                    message: "500 from Tailscale".into(),
                    maybe_applied: false,
                });
            }
            *self.service.lock().expect("lock") = Some(ServiceDefinition {
                addrs: vec!["100.100.1.5".into()],
                ..definition.clone()
            });
            Ok(())
        }
        async fn service_hosts(&self, name: &str) -> Result<Vec<ServiceHost>, String> {
            self.log(format!("service_hosts {name}"));
            Ok(self.service_hosts.lock().expect("lock").clone())
        }
        async fn service_entries(&self, name: &str) -> Result<Vec<ServiceEntryHost>, String> {
            self.log(format!("service_entries {name}"));
            Ok(self.service_entries.lock().expect("lock").clone())
        }
        async fn service_creation(
            &self,
            name: &str,
            creator_run_id: i32,
        ) -> Result<Option<(ServiceDefinition, temps_core::DBDateTime)>, String> {
            self.log(format!("service_creation {name} {creator_run_id}"));
            Ok(self.creation.clone())
        }
        async fn delete_service(
            &self,
            tailnet: &str,
            name: &str,
            _creator_run_id: i32,
            shown: &ServiceDefinition,
        ) -> Result<ServiceDeletion, String> {
            self.log(format!("delete_service {tailnet} {name} {}", shown.comment));
            Ok(self.deletion.clone().unwrap_or(ServiceDeletion::Deleted))
        }
        async fn sleep(&self, _duration: Duration) {}
    }

    #[derive(Default)]
    struct Saved(Mutex<Vec<RunRecord>>, bool);

    #[async_trait]
    impl RunSink for Saved {
        async fn save(&self, record: &RunRecord) -> Result<(), String> {
            if self.1 {
                return Err("disk full".into());
            }
            self.0.lock().expect("lock").push(record.clone());
            Ok(())
        }
    }

    fn timing() -> Timing {
        Timing {
            poll: Duration::from_millis(1),
            connect: Duration::from_millis(5),
            publish: Duration::from_millis(5),
            dns: Duration::from_millis(5),
        }
    }

    fn steps(kinds: &[StepKind]) -> Vec<PlanStep> {
        kinds
            .iter()
            .map(|kind| PlanStep {
                kind: *kind,
                label: format!("{kind:?}"),
            })
            .collect()
    }

    fn token_input() -> RunInput {
        RunInput {
            run_id: 0,
            route_confirmed: false,
            project_id: 1,
            environment_id: 7,
            environment_name: "production".into(),
            option: PlanOptionKind::NewTunnelWithToken,
            address: "myapp.example.com".into(),
            steps: steps(&[
                StepKind::CheckToken,
                StepKind::SaveDnsProvider,
                StepKind::ManageDomain,
                StepKind::CreateTunnel,
                StepKind::WaitForTunnel,
                StepKind::AttachDomain,
                StepKind::RouteHostname,
                StepKind::WaitForPublication,
                StepKind::CheckReachability,
            ]),
            tunnel: None,
            new_tunnel: Some(NewTunnel {
                provider_id: None,
                provider_name: "Cloudflare example.com".into(),
                zone: "example.com".into(),
                domain: "example.com".into(),
                name: "example-com".into(),
                worker: NamedId {
                    id: 2,
                    name: "master".into(),
                },
            }),
            token: Some("secret-token".into()),
            direct: None,
            acme_email: None,
            dns_before: PlannedRecord::Absent,
            certificate_challenge: None,
            audience: None,
            serve: None,
            tailscale_service: None,
            tailscale_credential: None,
        }
    }

    #[tokio::test]
    async fn a_token_run_records_everything_it_creates_in_order() {
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        let sink = Saved::default();
        let record = execute(&token_input(), &ops, &sink, timing()).await;
        assert_eq!(record.state, RunState::Succeeded);
        assert_eq!(
            record.live_url.as_deref(),
            Some("https://myapp.example.com")
        );
        assert_eq!(
            record.created(),
            vec![
                RunResource::DnsProvider {
                    id: 30,
                    name: "Cloudflare example.com".into()
                },
                RunResource::ManagedDomain {
                    provider_id: 30,
                    domain: "example.com".into()
                },
                RunResource::Connector {
                    id: 40,
                    name: "example-com".into()
                },
                RunResource::CustomDomain {
                    id: 50,
                    domain: "myapp.example.com".into()
                },
                RunResource::HostnameChoice {
                    hostname: "myapp.example.com".into(),
                    connector_id: 40,
                    previous: PreviousChoice::Automatic,
                },
            ]
        );
        assert!(record.steps.iter().all(|s| s.status == RunStepStatus::Done));
        // Progress was saved as it happened, and the token never was.
        let saved = sink.0.lock().expect("lock");
        assert!(saved.len() > record.steps.len());
        assert!(!format!("{saved:?}").contains("secret-token"));
    }

    #[tokio::test]
    async fn a_failed_step_undoes_the_run_newest_first() {
        let ops = Fake {
            fail: vec!["attach_domain"],
            ..Default::default()
        };
        let record = execute(&token_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Failed);
        let error = record.error.expect("error");
        assert!(error.starts_with("attach_domain broke"), "{error}");
        assert!(error.contains("was removed"));
        let undo_calls: Vec<String> = ops
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("delete_") || c.starts_with("remove_"))
            .collect();
        assert_eq!(
            undo_calls,
            vec![
                "delete_tunnel 40",
                "remove_managed_zone example.com",
                "delete_dns_provider 30"
            ]
        );
        assert!(record.undo.iter().all(|u| u.ok));
        let attach = record
            .steps
            .iter()
            .find(|s| s.kind == StepKind::AttachDomain)
            .expect("step");
        assert_eq!(attach.status, RunStepStatus::Failed);
    }

    #[tokio::test]
    async fn undo_leaves_alone_what_the_run_found_already_there() {
        // The zone was managed with automatic DNS off; the domain was already
        // on this environment and already chosen for the tunnel.
        let ops = Fake {
            zone: Some(ZoneState {
                auto_manage: false,
                verified: true,
            }),
            existing_domain: Some((51, 7)),
            choice: Some(Some(40)),
            reachable: true,
            ..Default::default()
        };
        let mut input = token_input();
        input.new_tunnel = Some(NewTunnel {
            provider_id: Some(3),
            ..input.new_tunnel.clone().expect("tunnel")
        });
        input.token = None;
        input.option = PlanOptionKind::NewTunnel;
        input
            .steps
            .retain(|s| !matches!(s.kind, StepKind::CheckToken | StepKind::SaveDnsProvider));
        let record = execute(&input, &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded);
        assert_eq!(
            record.created(),
            vec![
                RunResource::AutoManageEnabled {
                    provider_id: 3,
                    domain: "example.com".into()
                },
                RunResource::Connector {
                    id: 40,
                    name: "example-com".into()
                },
            ]
        );
        let mut journal = record.journal.clone();
        let results = undo(&mut journal, &ops, 0, &Journaled::default())
            .await
            .results;
        assert_eq!(
            results.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            vec![
                "Removed Tunnel example-com",
                "Turned automatic DNS for example.com back off"
            ]
        );
        assert!(!ops.calls().iter().any(|c| c.starts_with("detach_domain")));
    }

    #[tokio::test]
    async fn an_undo_failure_is_reported_and_the_rest_still_removed() {
        let ops = Fake {
            fail: vec!["detach_domain"],
            ..Default::default()
        };
        let mut journal = vec![
            done(
                Intent::DnsProvider { name: "cf".into() },
                RunResource::DnsProvider {
                    id: 30,
                    name: "cf".into(),
                },
            ),
            done(
                Intent::CustomDomain {
                    domain: "myapp.example.com".into(),
                    environment_id: 7,
                },
                RunResource::CustomDomain {
                    id: 50,
                    domain: "myapp.example.com".into(),
                },
            ),
        ];
        let results = undo(&mut journal, &ops, 0, &Journaled::default())
            .await
            .results;
        assert!(!results[0].ok);
        assert_eq!(results[0].error.as_deref(), Some("database is down"));
        assert!(results[1].ok);
    }

    /// A tunnel that could not be removed stops undo: its DNS provider,
    /// older, is what removing it needs. It stays `deleting`.
    #[tokio::test]
    async fn a_tunnel_that_could_not_be_removed_keeps_what_it_needs() {
        let ops = Fake {
            fail: vec!["delete_tunnel"],
            ..Default::default()
        };
        let mut journal = vec![
            done(
                Intent::DnsProvider { name: "cf".into() },
                RunResource::DnsProvider {
                    id: 30,
                    name: "cf".into(),
                },
            ),
            done(
                Intent::Connector { name: "t".into() },
                RunResource::Connector {
                    id: 40,
                    name: "t".into(),
                },
            ),
        ];
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert_eq!(outcome.results.len(), 1);
        assert!(!outcome.results[0].ok);
        assert!(
            matches!(&outcome.stopped, Some(Stop::Failed(why)) if why.contains("never brings it back"))
        );
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("delete_dns_provider")));
        assert!(journal[1].deleting);
        assert_eq!(journal[1].state, JournalState::Done);

        // Undo again: it is not marked twice, and finishes.
        let ops = Fake::default();
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert!(outcome.stopped.is_none());
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("mark_tunnel_deleting")));
        assert_eq!(
            ops.calls()
                .into_iter()
                .filter(|c| c.starts_with("delete_"))
                .collect::<Vec<_>>(),
            vec!["delete_tunnel 40", "delete_dns_provider 30"]
        );
    }

    /// ADR 064 D4: the tunnel is marked `deleting`, and that journaled,
    /// before the provider tunnel is touched.
    #[tokio::test]
    async fn undo_marks_a_tunnel_deleting_and_journals_it_before_removing_it() {
        let ops = Fake::default();
        let mut journal = vec![done(
            Intent::Connector { name: "t".into() },
            RunResource::Connector {
                id: 40,
                name: "t".into(),
            },
        )];
        let sink = Journaled::default();
        let outcome = undo(&mut journal, &ops, 0, &sink).await;
        assert!(outcome.stopped.is_none());
        let calls = ops.calls();
        let mark = calls
            .iter()
            .position(|c| c == "mark_tunnel_deleting 40")
            .expect("marked");
        let delete = calls
            .iter()
            .position(|c| c == "delete_tunnel 40")
            .expect("deleted");
        assert!(mark < delete);
        // The provider tunnel id read when marking is what is deleted.
        assert!(calls.contains(&"tunnel_id cf-40".to_string()));
        let saved = sink.0.lock().expect("lock");
        assert!(saved[0][0].deleting, "journaled as deleting first");
        assert_eq!(saved[0][0].state, JournalState::Done);
        assert_eq!(journal[0].state, JournalState::Absent);
    }

    fn tunnel_journal() -> Vec<JournalEntry> {
        vec![
            done(
                Intent::DnsProvider { name: "cf".into() },
                RunResource::DnsProvider {
                    id: 30,
                    name: "cf".into(),
                },
            ),
            done(
                Intent::Connector { name: "t".into() },
                RunResource::Connector {
                    id: 40,
                    name: "t".into(),
                },
            ),
            done(
                Intent::HostnameChoice {
                    hostname: "myapp.example.com".into(),
                    connector_id: 40,
                    previous: PreviousChoice::Automatic,
                },
                RunResource::HostnameChoice {
                    hostname: "myapp.example.com".into(),
                    connector_id: 40,
                    previous: PreviousChoice::Automatic,
                },
            ),
        ]
    }

    /// ADR 064 D4 (fourth amendment): sync published another hostname on
    /// the run's tunnel through auto-publish. Undo pauses naming it, and
    /// touches neither the tunnel nor what it runs on until the user says.
    #[tokio::test]
    async fn a_tunnel_serving_a_hostname_the_run_never_journaled_pauses_undo() {
        let ops = Fake::default();
        *ops.served.lock().expect("lock") =
            vec!["myapp.example.com".into(), "Other.Example.com.".into()];
        let mut journal = tunnel_journal();
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        let Some(Stop::Conflict(why)) = &outcome.stopped else {
            panic!("expected a conflict, got {:?}", outcome.stopped);
        };
        assert!(
            why.contains("Tunnel t now also publishes other.example.com"),
            "{why}"
        );
        // The run's own hostname choice, newer, was put back.
        assert_eq!(outcome.results.len(), 1);
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("mark_tunnel_deleting") || c.starts_with("delete_")));
        assert_eq!(journal[1].state, JournalState::Conflict);

        // Keep: the tunnel stays, and so does the DNS provider it uses.
        let mut kept = journal.clone();
        let key = journal[1].key.clone();
        decide(&mut kept, &key, Decision::Keep, true).expect("decided");
        let outcome = undo(&mut kept, &ops, 0, &Journaled::default()).await;
        assert!(outcome.stopped.is_none());
        assert!(!ops.calls().iter().any(|c| c.starts_with("delete_")));
        assert!(
            outcome.results[0]
                .label
                .starts_with("Kept Cloudflare account cf")
                || outcome.results[0].label.starts_with("Kept"),
            "{:?}",
            outcome.results
        );
        assert_eq!(kept[1].undo_decision, Some(Decision::Keep));

        // Remove anyway: journaled, then removed with what it runs on.
        let ops = Fake::default();
        *ops.served.lock().expect("lock") = vec!["other.example.com".into()];
        let key = journal[1].key.clone();
        decide(&mut journal, &key, Decision::Overwrite, true).expect("decided");
        assert_eq!(journal[1].undo_decision, Some(Decision::Overwrite));
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert!(outcome.stopped.is_none(), "{:?}", outcome.stopped);
        assert_eq!(
            ops.calls()
                .into_iter()
                .filter(|c| c.starts_with("delete_"))
                .collect::<Vec<_>>(),
            vec!["delete_tunnel 40", "delete_dns_provider 30"]
        );
    }

    fn created_record(content: &str) -> JournalEntry {
        done(
            Intent::DnsRecord {
                hostname: "myapp.example.com".into(),
                label: String::new(),
                record_type: "A".into(),
            },
            RunResource::DnsRecord {
                hostname: "myapp.example.com".into(),
                label: "myapp".into(),
                record_type: "A".into(),
                content: Some(content.into()),
            },
        )
    }

    /// ADR 064 D4: undo removes a record it created only while it still
    /// holds what this run wrote, with this run's marker.
    #[tokio::test]
    async fn undo_never_deletes_a_record_without_the_run_marker() {
        // Another run (or a person, through Temps) wrote it since.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("203.0.113.5", Some(3), "later"));
        let mut journal = vec![created_record("203.0.113.5")];
        let outcome = undo(&mut journal, &ops, 9, &Journaled::default()).await;
        assert!(matches!(outcome.stopped, Some(Stop::Conflict(_))));
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("remove_dns_record")));
        // "Remove anyway" is not offered for a record the run did not write.
        let key = journal[0].key.clone();
        assert!(decide(&mut journal, &key, Decision::Overwrite, true).is_err());
        decide(&mut journal, &key, Decision::Keep, true).expect("kept");
        let outcome = undo(&mut journal, &ops, 9, &Journaled::default()).await;
        assert!(outcome.stopped.is_none());
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("remove_dns_record")));

        // Not managed by Temps at all: the same.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(DnsRecordState {
            managed: false,
            ..record("203.0.113.5", None, "later")
        });
        let mut journal = vec![created_record("203.0.113.5")];
        assert!(undo(&mut journal, &ops, 9, &Journaled::default())
            .await
            .stopped
            .is_some());
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("remove_dns_record")));

        // Still this run's: removed. Gone already: nothing to do.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("203.0.113.5", Some(9), "written"));
        let mut journal = vec![created_record("203.0.113.5")];
        assert!(undo(&mut journal, &ops, 9, &Journaled::default())
            .await
            .stopped
            .is_none());
        assert!(ops
            .calls()
            .contains(&"remove_dns_record myapp.example.com myapp A".to_string()));
        let ops = Fake::default();
        let mut journal = vec![created_record("203.0.113.5")];
        assert!(undo(&mut journal, &ops, 9, &Journaled::default())
            .await
            .stopped
            .is_none());
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("remove_dns_record")));
        assert_eq!(journal[0].state, JournalState::Absent);
    }

    fn direct_input(run_id: i32, dns_before: PlannedRecord) -> RunInput {
        RunInput {
            run_id,
            option: PlanOptionKind::DirectManagedDns,
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::SetDnsRecord,
                StepKind::CheckReachability,
            ]),
            new_tunnel: None,
            token: None,
            direct: Some(("edge-1".into(), "203.0.113.5".parse().expect("ip"))),
            dns_before,
            ..token_input()
        }
    }

    fn private_input(run_id: i32) -> RunInput {
        RunInput {
            run_id,
            option: PlanOptionKind::PrivateWorker,
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::SetAudience,
                StepKind::SetDnsRecord,
                StepKind::IssueCertificate,
                StepKind::CheckReachability,
            ]),
            new_tunnel: None,
            token: None,
            direct: Some(("homelab".into(), "100.101.102.103".parse().expect("ip"))),
            dns_before: PlannedRecord::Absent,
            certificate_challenge: Some("dns-01".into()),
            audience: Some((
                AddressAudience {
                    audience: Audience::Private,
                    network_range: Some("100.64.0.0/10".into()),
                    private_node_id: Some(2),
                    private_listener: Some("100.101.102.103".into()),
                    ..AddressAudience::default()
                },
                AddressAudience::public(),
            )),
            acme_email: Some("ops@example.com".into()),
            ..token_input()
        }
    }

    /// ADR 064 D5: a private run points DNS at the worker's private
    /// listener, issues over DNS-01 only, and records the audience with
    /// its range; undoing it removes the address it attached without
    /// making it public first.
    #[tokio::test]
    async fn a_private_run_points_at_the_listener_and_undo_never_makes_it_public() {
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        let record = execute(&private_input(12), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);
        let calls = ops.calls();
        assert!(
            calls.contains(&"write_dns_record 100.101.102.103 Some(12)".to_string()),
            "{calls:?}"
        );
        assert!(calls.contains(&"challenge dns-01".to_string()), "{calls:?}");
        assert!(
            calls.contains(&"set_address_audience private to 100.64.0.0/10".to_string()),
            "{calls:?}"
        );
        // The audience is set before DNS leads anyone there.
        let audience_at = calls
            .iter()
            .position(|c| c.starts_with("set_address_audience"))
            .expect("audience set");
        let dns_at = calls
            .iter()
            .position(|c| c.starts_with("write_dns_record"))
            .expect("dns written");
        assert!(audience_at < dns_at, "{calls:?}");

        let mut journal = record.journal.clone();
        let undo_ops = Fake::default();
        *undo_ops.dns.lock().expect("lock") = ops.dns.lock().expect("lock").clone();
        let outcome = undo(&mut journal, &undo_ops, 12, &Journaled::default()).await;
        assert!(outcome.stopped.is_none(), "{:?}", outcome.stopped);
        let undone = undo_ops.calls();
        assert!(
            !undone.iter().any(|c| c.starts_with("set_address_audience")),
            "an address being removed is never made public first: {undone:?}"
        );
        assert!(
            undone.iter().any(|c| c.starts_with("detach_domain")),
            "{undone:?}"
        );
        assert!(
            undone.iter().any(|c| c.starts_with("remove_dns_record")),
            "{undone:?}"
        );
    }

    /// Undo puts an existing address's audience back only while it still
    /// holds what the run set: one changed since, by anyone, is left as it
    /// is and undo stops there.
    #[tokio::test]
    async fn undo_never_reverts_an_audience_changed_after_the_run() {
        let ops = Fake {
            reachable: true,
            existing_domain: Some((51, 7)),
            ..Default::default()
        };
        let record = execute(&private_input(13), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);
        let set = ops
            .audience
            .lock()
            .expect("lock")
            .clone()
            .expect("audience set");

        // Still what the run set: it goes back to public.
        let mut journal = record.journal.clone();
        let undo_ops = Fake::default();
        *undo_ops.dns.lock().expect("lock") = ops.dns.lock().expect("lock").clone();
        *undo_ops.audience.lock().expect("lock") = Some(set.clone());
        let outcome = undo(&mut journal, &undo_ops, 13, &Journaled::default()).await;
        assert!(outcome.stopped.is_none(), "{:?}", outcome.stopped);
        assert!(
            undo_ops
                .calls()
                .contains(&"set_address_audience public".to_string()),
            "{:?}",
            undo_ops.calls()
        );

        // Changed since: left alone, and undo stops on it.
        let mut journal = record.journal.clone();
        let undo_ops = Fake::default();
        *undo_ops.dns.lock().expect("lock") = ops.dns.lock().expect("lock").clone();
        *undo_ops.audience.lock().expect("lock") = Some(AddressAudience {
            network_range: Some("10.0.0.0/8".into()),
            ..set
        });
        let outcome = undo(&mut journal, &undo_ops, 13, &Journaled::default()).await;
        assert!(
            matches!(outcome.stopped, Some(Stop::Conflict(_))),
            "{:?}",
            outcome.stopped
        );
        assert!(
            !undo_ops
                .calls()
                .iter()
                .any(|c| c.starts_with("set_address_audience")),
            "{:?}",
            undo_ops.calls()
        );
    }

    fn was() -> DnsRecordState {
        record("198.51.100.1", None, "before")
    }

    /// ADR 064 D4: the set-DNS step re-reads the record just before
    /// writing; changed since the plan, it writes nothing and stops naming
    /// the new value.
    #[tokio::test]
    async fn a_record_changed_after_planning_is_never_written_over() {
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        // Same content, but edited (and put back) since: still changed.
        *ops.dns.lock().expect("lock") = Some(record("198.51.100.7", None, "edited"));
        let input = direct_input(9, PlannedRecord::Present { record: was() });
        let record_after = execute(&input, &ops, &Saved::default(), timing()).await;
        assert_eq!(record_after.state, RunState::Conflict);
        let error = record_after.error.clone().expect("error");
        assert!(
            error.contains(
                "changed at your DNS provider after this plan was made: it is now A \
                            198.51.100.7"
            ),
            "{error}"
        );
        assert!(error.contains("Temps wrote nothing"));
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("write_dns_record")));
        // Nothing is undone: undo could put back what someone just changed.
        assert!(record_after.undo.is_empty());
        // The conflict was found before writing, so undo has nothing to put
        // back for it and goes past it.
        let mut journal = record_after.journal.clone();
        let outcome = undo(&mut journal, &ops, 9, &Journaled::default()).await;
        assert!(outcome.stopped.is_none(), "{:?}", outcome.stopped);
        assert!(ops.calls().contains(&"detach_domain 50".to_string()));

        // A record that appeared where the plan saw none: the same.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(was());
        let record_after = execute(
            &direct_input(9, PlannedRecord::Absent),
            &ops,
            &Saved::default(),
            timing(),
        )
        .await;
        assert_eq!(record_after.state, RunState::Conflict);
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("write_dns_record")));

        // Keep: resuming skips the record. Overwrite: it is set, with the
        // value there now journaled as `before`.
        let mut kept = record_after.clone();
        let key = kept.conflicts()[0].key.clone();
        decide(&mut kept.journal, &key, Decision::Keep, false).expect("decided");
        let ops_keep = Fake {
            reachable: true,
            ..Default::default()
        };
        *ops_keep.dns.lock().expect("lock") = Some(was());
        let input = direct_input(9, PlannedRecord::Absent);
        let resumed = resume(&input, kept, &ops_keep, &Saved::default(), timing()).await;
        assert_eq!(resumed.state, RunState::Succeeded);
        assert!(!ops_keep
            .calls()
            .iter()
            .any(|c| c.starts_with("write_dns_record")));

        let mut overwrite = record_after;
        decide(&mut overwrite.journal, &key, Decision::Overwrite, false).expect("decided");
        let ops_over = Fake {
            reachable: true,
            ..Default::default()
        };
        *ops_over.dns.lock().expect("lock") = Some(was());
        let resumed = resume(&input, overwrite, &ops_over, &Saved::default(), timing()).await;
        assert_eq!(resumed.state, RunState::Succeeded);
        assert!(ops_over
            .calls()
            .contains(&"write_dns_record 203.0.113.5 Some(9)".to_string()));
        assert!(resumed.created().iter().any(|r| matches!(
            r,
            RunResource::DnsChange { before, after, .. }
                if before.content == "198.51.100.1" && after == "203.0.113.5"
        )));
    }

    /// ADR 064 D4: a change journals the value read just before the write
    /// and the one it writes, stamped with the run's marker; undo puts the
    /// old one back only while the record holds exactly this run's.
    #[tokio::test]
    async fn undo_restores_a_changed_record_only_while_it_holds_what_the_run_wrote() {
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        *ops.dns.lock().expect("lock") = Some(was());
        let input = direct_input(9, PlannedRecord::Present { record: was() });
        let done_run = execute(&input, &ops, &Saved::default(), timing()).await;
        assert_eq!(done_run.state, RunState::Succeeded);
        assert!(ops
            .calls()
            .contains(&"write_dns_record 203.0.113.5 Some(9)".to_string()));
        let change = done_run
            .journal
            .iter()
            .find(|e| matches!(e.intent, Intent::DnsChange { .. }))
            .expect("change journaled");
        let Intent::DnsChange { before, after, .. } = &change.intent else {
            unreachable!()
        };
        assert_eq!(before, &was());
        assert_eq!(after, "203.0.113.5");

        // Untouched since: restored, and its earlier marker with it.
        let mut journal = done_run.journal.clone();
        let outcome = undo(&mut journal, &ops, 9, &Journaled::default()).await;
        assert!(outcome.stopped.is_none());
        assert!(ops
            .calls()
            .contains(&"write_dns_record 198.51.100.1 None".to_string()));
        assert!(outcome
            .results
            .iter()
            .any(|r| r.label == "Put myapp.example.com back to A 198.51.100.1"));

        // Changed again since: nothing written, undo pauses there, and the
        // older step (the custom domain) is left alone.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("192.0.2.44", None, "later"));
        let mut journal = done_run.journal.clone();
        let outcome = undo(&mut journal, &ops, 9, &Journaled::default()).await;
        let Some(Stop::Conflict(why)) = &outcome.stopped else {
            panic!("expected a conflict, got {:?}", outcome.stopped);
        };
        assert!(why.contains("it is now A 192.0.2.44"), "{why}");
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("write_dns_record")));
        assert!(!ops.calls().iter().any(|c| c.starts_with("detach_domain")));

        // Skip it: the rest is undone, the record left as they set it.
        let key = journal
            .iter()
            .find(|e| e.state == JournalState::Conflict)
            .expect("conflict")
            .key
            .clone();
        let mut skipped = journal.clone();
        decide(&mut skipped, &key, Decision::Keep, true).expect("decided");
        let outcome = undo(&mut skipped, &ops, 9, &Journaled::default()).await;
        assert!(outcome.stopped.is_none());
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("write_dns_record")));
        assert!(ops.calls().contains(&"detach_domain 50".to_string()));
        assert_eq!(
            skipped
                .iter()
                .find(|e| e.key == key)
                .expect("entry")
                .undo_decision,
            Some(Decision::Keep)
        );

        // Restore anyway: journaled, and written.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("192.0.2.44", None, "later"));
        decide(&mut journal, &key, Decision::Overwrite, true).expect("decided");
        let outcome = undo(&mut journal, &ops, 9, &Journaled::default()).await;
        assert!(outcome.stopped.is_none());
        assert!(ops
            .calls()
            .contains(&"write_dns_record 198.51.100.1 None".to_string()));
    }

    fn pending_change(since: temps_core::DBDateTime) -> JournalEntry {
        let intent = Intent::DnsChange {
            hostname: "myapp.example.com".into(),
            label: "myapp".into(),
            record_type: "A".into(),
            before: was(),
            after: "203.0.113.5".into(),
        };
        JournalEntry::pending(intent.key(9), intent, since)
    }

    /// ADR 064 D4: reconciling a change: this run's after with its marker
    /// is done; still the before is a write that did not land (confirmed by
    /// two misses a settle delay apart), retried through the pre-write
    /// check; anything else is a conflict nothing touches.
    #[tokio::test]
    async fn reconciling_a_change_tells_done_retry_and_conflict_apart() {
        let now = chrono::Utc::now();
        let since = now - chrono::Duration::minutes(2);

        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("203.0.113.5", Some(9), "written"));
        let mut journal = vec![pending_change(since)];
        let out = reconcile_journal(&mut journal, &ops, 9, now).await;
        assert_eq!(out.resolved, 1);
        assert_eq!(journal[0].state, JournalState::Done);
        assert!(matches!(
            journal[0].resource,
            Some(RunResource::DnsChange { .. })
        ));

        // The same content written by another run is not this run's.
        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("203.0.113.5", Some(8), "written"));
        let mut journal = vec![pending_change(since)];
        reconcile_journal(&mut journal, &ops, 9, now).await;
        assert_eq!(journal[0].state, JournalState::Conflict);

        let ops = Fake::default();
        *ops.dns.lock().expect("lock") = Some(record("192.0.2.44", None, "later"));
        let mut journal = vec![pending_change(since)];
        let out = reconcile_journal(&mut journal, &ops, 9, now).await;
        assert_eq!(out.conflicts, 1);
        assert_eq!(journal[0].state, JournalState::Conflict);
        assert_eq!(
            journal[0].conflict.as_deref(),
            Some("A 192.0.2.44 (set by Temps)")
        );

        // Still the before content: absent only after a second look.
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        *ops.dns.lock().expect("lock") = Some(was());
        let mut journal = vec![pending_change(since)];
        assert_eq!(
            reconcile_journal(&mut journal, &ops, 9, now).await.pending,
            1
        );
        assert_eq!(journal[0].first_miss_at, Some(now));
        let later = now + chrono::Duration::seconds(31);
        assert_eq!(
            reconcile_journal(&mut journal, &ops, 9, later)
                .await
                .resolved,
            1
        );
        assert_eq!(journal[0].state, JournalState::Absent);
        // ...and a resume retries it through the pre-write check.
        let input = direct_input(9, PlannedRecord::Present { record: was() });
        let mut stopped = RunRecord::new(&input.steps);
        stopped.journal = journal;
        stopped.state = RunState::Interrupted;
        let resumed = resume(&input, stopped, &ops, &Saved::default(), timing()).await;
        assert_eq!(resumed.state, RunState::Succeeded);
        let calls = ops.calls();
        let read = calls
            .iter()
            .rposition(|c| c == "read_dns_record")
            .expect("read");
        let write = calls
            .iter()
            .position(|c| c == "write_dns_record 203.0.113.5 Some(9)")
            .expect("written");
        assert!(read < write);
    }

    /// ADR 064 D4: a miss proves nothing until the create's timeout and a
    /// settle delay have passed and a second miss confirms it; until then
    /// the step stays pending and the create is never repeated.
    #[tokio::test]
    async fn an_ambiguous_miss_never_creates_the_record_twice() {
        // The write timed out; nothing is visible yet.
        let ops = Fake {
            fail: vec!["write_dns_record"],
            ..Default::default()
        };
        let input = direct_input(9, PlannedRecord::Absent);
        let failed = execute(&input, &ops, &Saved::default(), timing()).await;
        let writes = |ops: &Fake| {
            ops.calls()
                .iter()
                .filter(|c| c.starts_with("write_dns_record"))
                .count()
        };
        assert_eq!(writes(&ops), 1);
        // Undo cannot go past a step it cannot tell about.
        assert_eq!(failed.state, RunState::Recovering, "{:?}", failed.error);
        assert!(failed.has_pending());
        assert!(!ops.calls().iter().any(|c| c.starts_with("detach_domain")));

        // A resume while it is still ambiguous waits; it never writes again.
        let mut stopped = failed.clone();
        stopped.state = RunState::Interrupted;
        let ops = Fake::default();
        let resumed = resume(&input, stopped, &ops, &Saved::default(), timing()).await;
        assert_eq!(resumed.state, RunState::Waiting);
        assert!(resumed.error.expect("why").contains("never makes it twice"));
        assert_eq!(writes(&ops), 0);

        // The reconciler: too soon, a first miss, too soon again, confirmed.
        let mut journal = failed.journal.clone();
        let entry = journal
            .iter()
            .find(|e| e.state == JournalState::Pending)
            .expect("pending")
            .clone();
        let since = entry.pending_since.expect("since");
        let at = |s: i64| since + chrono::Duration::seconds(s);
        assert_eq!(
            reconcile_journal(&mut journal, &ops, 9, at(10))
                .await
                .pending,
            1
        );
        assert_eq!(
            reconcile_journal(&mut journal, &ops, 9, at(61))
                .await
                .pending,
            1
        );
        assert_eq!(
            reconcile_journal(&mut journal, &ops, 9, at(80))
                .await
                .pending,
            1
        );
        assert_eq!(
            reconcile_journal(&mut journal, &ops, 9, at(92))
                .await
                .resolved,
            1
        );
        assert!(!journal.iter().any(|e| e.state == JournalState::Pending));
        assert_eq!(writes(&ops), 0);

        // Still unresolved after an hour, it says what to check by hand.
        let mut journal = failed.journal.clone();
        let broken = Fake {
            fail: vec!["read_dns_record"],
            ..Default::default()
        };
        let out = reconcile_journal(&mut journal, &broken, 9, at(3700)).await;
        assert_eq!(out.pending, 1);
        assert!(out.check_by_hand[0].contains("Check the A record for `myapp.example.com`"));
    }

    /// A tunnel's lookup is its Temps row, saved before Cloudflare is
    /// asked: authoritative at once.
    #[test]
    fn a_tunnel_lookup_is_authoritative_and_dns_follows_the_two_miss_rule() {
        assert_eq!(
            Intent::Connector { name: "t".into() }.authority(),
            LookupAuthority::Immediate
        );
        assert!(matches!(
            Intent::DnsRecord {
                hostname: "a".into(),
                label: String::new(),
                record_type: "A".into()
            }
            .authority(),
            LookupAuthority::Settled { .. }
        ));
    }

    #[tokio::test]
    async fn a_missing_permission_stops_before_anything_is_created() {
        let ops = Fake {
            fail: vec!["check_token"],
            ..Default::default()
        };
        let record = execute(&token_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Failed);
        assert!(record.created().is_empty());
        assert!(record
            .error
            .expect("error")
            .contains("The token lacks Zone · DNS · Edit"));
        assert!(!ops.calls().contains(&"save_dns_provider".to_string()));
    }

    #[tokio::test]
    async fn slow_publication_keeps_everything_and_waits() {
        let ops = Fake {
            published_after: usize::MAX,
            ..Default::default()
        };
        let record = execute(&token_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Waiting);
        assert_eq!(record.created().len(), 5);
        assert!(record.undo.is_empty());
        assert!(record.error.expect("reason").contains("Not published yet"));
        let check = record.steps.last().expect("step");
        assert_eq!(check.status, RunStepStatus::Pending);
    }

    #[tokio::test]
    async fn a_direct_run_sets_dns_and_issues_the_certificate() {
        let ops = Fake {
            reachable: true,
            resolves: true,
            ..Default::default()
        };
        let input = RunInput {
            option: PlanOptionKind::DirectManagedDns,
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::SetDnsRecord,
                StepKind::IssueCertificate,
                StepKind::CheckReachability,
            ]),
            new_tunnel: None,
            token: None,
            direct: Some(("edge-1".into(), "203.0.113.5".parse().expect("ip"))),
            acme_email: Some("ops@example.com".into()),
            ..token_input()
        };
        let record = execute(&input, &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded);
        assert_eq!(
            record.created(),
            vec![
                RunResource::CustomDomain {
                    id: 50,
                    domain: "myapp.example.com".into()
                },
                RunResource::DnsRecord {
                    hostname: "myapp.example.com".into(),
                    label: "myapp".into(),
                    record_type: "A".into(),
                    content: Some("203.0.113.5".into()),
                },
                RunResource::Certificate {
                    domain: "myapp.example.com".into()
                },
            ]
        );

        // Without an email the certificate step fails and everything goes.
        let ops = Fake::default();
        let record = execute(
            &RunInput {
                acme_email: None,
                ..input.clone()
            },
            &ops,
            &Saved::default(),
            timing(),
        )
        .await;
        assert_eq!(record.state, RunState::Failed);
        assert!(ops
            .calls()
            .contains(&"remove_dns_record myapp.example.com myapp A".to_string()));
    }

    #[tokio::test]
    async fn a_dns_record_the_user_has_not_added_yet_waits() {
        let ops = Fake::default();
        let input = RunInput {
            option: PlanOptionKind::DirectManualDns,
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::ShowDnsRecord,
                StepKind::IssueCertificate,
                StepKind::CheckReachability,
            ]),
            new_tunnel: None,
            token: None,
            direct: Some(("control-plane".into(), "203.0.113.5".parse().expect("ip"))),
            ..token_input()
        };
        let record = execute(&input, &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Waiting);
        assert_eq!(record.created().len(), 1);
        assert!(record
            .error
            .expect("reason")
            .contains("Add an A record for myapp.example.com pointing at 203.0.113.5"));
    }

    #[tokio::test]
    async fn every_creating_call_is_journaled_pending_first() {
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        let sink = Saved::default();
        execute(&token_input(), &ops, &sink, timing()).await;
        let saved = sink.0.lock().expect("lock");
        // The tunnel's entry was stored pending before it was created, and
        // done after.
        let key = Intent::Connector {
            name: "example-com".into(),
        }
        .key(0);
        let states: Vec<JournalState> = saved
            .iter()
            .filter_map(|r| r.journal.iter().find(|e| e.key == key).map(|e| e.state))
            .collect();
        assert_eq!(states.first(), Some(&JournalState::Pending));
        assert_eq!(states.last(), Some(&JournalState::Done));
    }

    #[tokio::test]
    async fn nothing_is_created_when_the_pending_entry_cannot_be_stored() {
        let ops = Fake::default();
        let sink = Saved(Mutex::new(Vec::new()), true);
        let record = execute(&token_input(), &ops, &sink, timing()).await;
        assert_eq!(record.state, RunState::Failed);
        assert!(!ops.calls().contains(&"save_dns_provider".to_string()));
        let error = record.error.expect("error");
        assert!(error.contains("could not record"), "{error}");
    }

    #[tokio::test]
    async fn a_resume_finds_what_an_interrupted_attempt_made_instead_of_making_it_twice() {
        // Temps stopped after the tunnel was created but before its entry
        // was marked done.
        let ops = Fake {
            reachable: true,
            ..Default::default()
        };
        let input = token_input();
        let mut record = RunRecord::new(&input.steps);
        record.journal = vec![
            done(
                Intent::DnsProvider {
                    name: "Cloudflare example.com".into(),
                },
                RunResource::DnsProvider {
                    id: 30,
                    name: "Cloudflare example.com".into(),
                },
            ),
            done(
                Intent::ManagedDomain {
                    provider_id: 30,
                    domain: "example.com".into(),
                },
                RunResource::ManagedDomain {
                    provider_id: 30,
                    domain: "example.com".into(),
                },
            ),
            JournalEntry {
                state: JournalState::Pending,
                resource: None,
                ..done(
                    Intent::Connector {
                        name: "example-com".into(),
                    },
                    RunResource::Connector {
                        id: 0,
                        name: String::new(),
                    },
                )
            },
        ];
        for step in record.steps.iter_mut().take(3) {
            step.status = RunStepStatus::Done;
        }
        record.state = RunState::Interrupted;
        ops.exists(
            &Intent::Connector {
                name: "example-com".into(),
            },
            RunResource::Connector {
                id: 40,
                name: "example-com".into(),
            },
        );
        let record = resume(&input, record, &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded);
        assert!(!ops.calls().contains(&"create_tunnel".to_string()));
        assert!(!ops.calls().contains(&"save_dns_provider".to_string()));
        // The found tunnel carries on: it is routed and waited for.
        assert!(ops.calls().contains(&"route_hostname 40".to_string()));
        assert_eq!(record.created().len(), 5);
        assert!(!record.has_pending());
    }

    #[tokio::test]
    async fn the_reconciler_resolves_pending_entries_by_lookup() {
        let ops = Fake::default();
        let tunnel = Intent::Connector { name: "t".into() };
        let domain = Intent::CustomDomain {
            domain: "myapp.example.com".into(),
            environment_id: 7,
        };
        ops.exists(
            &tunnel,
            RunResource::Connector {
                id: 40,
                name: "t".into(),
            },
        );
        let pending =
            |intent: Intent| JournalEntry::pending(intent.key(0), intent, chrono::Utc::now());
        let mut journal = vec![pending(tunnel), pending(domain)];
        let out = reconcile_journal(&mut journal, &ops, 0, chrono::Utc::now()).await;
        assert_eq!(out.resolved, 2);
        assert_eq!(journal[0].state, JournalState::Done);
        assert_eq!(journal[1].state, JournalState::Absent);

        // When the lookup itself fails the entry stays pending.
        let broken = Fake {
            fail: vec!["find"],
            ..Default::default()
        };
        let mut journal = vec![pending(Intent::Connector { name: "t".into() })];
        let out = reconcile_journal(&mut journal, &broken, 0, chrono::Utc::now()).await;
        assert_eq!(out.pending, 1);
        assert_eq!(journal[0].state, JournalState::Pending);
        // ...and backs off before looking again.
        assert_eq!(journal[0].failed_lookups, 1);
        assert!(journal[0].next_lookup_at.is_some());
    }

    #[tokio::test]
    async fn a_confirmed_route_records_and_undo_restores_the_previous_choice() {
        let ops = Fake {
            choice: Some(None),
            reachable: true,
            ..Default::default()
        };
        let input = RunInput {
            option: PlanOptionKind::ExistingTunnel,
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::RouteHostname,
                StepKind::WaitForPublication,
                StepKind::CheckReachability,
            ]),
            tunnel: Some(NamedId {
                id: 41,
                name: "homelab".into(),
            }),
            new_tunnel: None,
            token: None,
            route_confirmed: true,
            ..token_input()
        };
        let mut record = execute(&input, &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded);
        assert!(record.created().contains(&RunResource::HostnameChoice {
            hostname: "myapp.example.com".into(),
            connector_id: 41,
            previous: PreviousChoice::Off,
        }));
        let results = undo(&mut record.journal, &ops, 0, &Journaled::default())
            .await
            .results;
        assert!(results.iter().all(|r| r.ok));
        assert!(ops
            .calls()
            .contains(&"restore_hostname_choice myapp.example.com Off".to_string()));
        assert!(record.created().is_empty());

        // Unconfirmed, another choice is never overridden.
        let ops = Fake {
            choice: Some(None),
            ..Default::default()
        };
        let record = execute(
            &RunInput {
                route_confirmed: false,
                ..input
            },
            &ops,
            &Saved::default(),
            timing(),
        )
        .await;
        assert_eq!(record.state, RunState::Failed);
        assert!(!ops.calls().iter().any(|c| c.starts_with("route_hostname")));
    }

    #[test]
    fn the_token_is_never_serialized_with_the_input() {
        let input = token_input();
        let json = serde_json::to_string(&input).expect("json");
        assert!(!json.contains("secret-token"));
        let back: RunInput = serde_json::from_str(&json).expect("input");
        assert_eq!(back.token, None);
        assert_eq!(back.address, input.address);
    }

    fn serve_input() -> RunInput {
        RunInput {
            option: PlanOptionKind::TailscaleServe,
            address: "homelab.example.ts.net".into(),
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::SetAudience,
                StepKind::ConfigureTailscaleServe,
                StepKind::CheckReachability,
            ]),
            new_tunnel: None,
            token: None,
            audience: Some((
                AddressAudience {
                    audience: Audience::Private,
                    private_node_id: Some(2),
                    private_via: Some("tailscale_serve".into()),
                    ..AddressAudience::default()
                },
                AddressAudience::public(),
            )),
            serve: Some(ServeTarget {
                node_id: 2,
                node_name: "homelab".into(),
                via: "tailscale_serve".into(),
                service: None,
                tailnet: Some("example.ts.net".into()),
            }),
            ..token_input()
        }
    }

    fn applied() -> ServeEntryStatus {
        ServeEntryStatus {
            entry_id: 70,
            report: Some(REPORT_APPLIED.into()),
            applied_etag: Some("\"e1\"".into()),
            ..ServeEntryStatus::default()
        }
    }

    fn serve_resource(record: &RunRecord) -> RunResource {
        record
            .created()
            .into_iter()
            .find(|r| matches!(r, RunResource::TailscaleServe { .. }))
            .expect("a Serve entry")
    }

    /// ADR 065 D4: the run declares the entry with its reference, and the
    /// step is done only once the worker's agent applied it. No DNS record
    /// and no certificate.
    #[tokio::test]
    async fn a_machine_name_run_declares_the_entry_and_waits_for_applied() {
        let ops = Fake {
            reachable: true,
            serve_status: Mutex::new(Some(applied())),
            ..Default::default()
        };
        let record = execute(&serve_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);
        assert_eq!(
            serve_resource(&record),
            RunResource::TailscaleServe {
                node_id: 2,
                node_name: "homelab".into(),
                address_id: "custom:50".into(),
                hostname: "homelab.example.ts.net".into(),
                entry_id: 70,
                created: true,
            }
        );
        let calls = ops.calls();
        assert!(
            calls.contains(&"claim_serve_entry homelab custom:50 homelab.example.ts.net".into())
        );
        assert!(!calls.iter().any(|c| c.starts_with("write_dns_record")
            || c.starts_with("challenge")
            || c == "certificate_exists"));
        assert_eq!(
            ops.audience
                .lock()
                .expect("lock")
                .as_ref()
                .map(|a| a.describe()),
            Some("private to your tailnet".into())
        );
    }

    /// ADR 065 D4: the agent never overwrites a slot it did not write, so
    /// the run stops in conflict with D5's message and nothing to decide:
    /// the worker's owner frees it, then resumes.
    #[tokio::test]
    async fn a_taken_slot_stops_the_run_in_conflict_with_nothing_to_decide() {
        let ops = Fake {
            serve_status: Mutex::new(Some(ServeEntryStatus {
                entry_id: 70,
                report: Some(REPORT_CONFLICT.into()),
                detail: Some("http://127.0.0.1:3000".into()),
                ..ServeEntryStatus::default()
            })),
            ..Default::default()
        };
        let record = execute(&serve_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Conflict);
        assert_eq!(
            record.error.as_deref(),
            Some(
                "`homelab.example.ts.net` already serves `http://127.0.0.1:3000` (set with \
                 `tailscale serve`, not by Temps). Remove it with `tailscale serve --https=443 \
                 off`, or give this app a Service name."
            )
        );
        assert!(record.conflicts().is_empty());
        // The entry is the run's, so undo can take it back.
        assert!(matches!(
            serve_resource(&record),
            RunResource::TailscaleServe { .. }
        ));

        // Funnel on the name stops it the same way.
        let funnel = Fake {
            serve_status: Mutex::new(Some(ServeEntryStatus {
                funnel: true,
                ..applied()
            })),
            ..Default::default()
        };
        let record = execute(&serve_input(), &funnel, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Conflict);
        assert!(record
            .error
            .unwrap_or_default()
            .contains("Tailscale Funnel"));

        // Another setup's entry for the slot: nothing is claimed.
        let differs = Fake {
            serve_claim: Some(ServeClaim::Differs("`svc:myapp`".into())),
            ..Default::default()
        };
        let record = execute(&serve_input(), &differs, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Conflict);
        assert!(!record
            .created()
            .iter()
            .any(|r| matches!(r, RunResource::TailscaleServe { .. })));

        // An agent that never answers: waiting, not failed.
        let silent = Fake {
            serve_status: Mutex::new(Some(ServeEntryStatus {
                entry_id: 70,
                worker_error: Some("Tailscale on `homelab` is `NeedsLogin`.".into()),
                ..ServeEntryStatus::default()
            })),
            ..Default::default()
        };
        let record = execute(&serve_input(), &silent, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Waiting, "{:?}", record.error);
        assert!(record.error.unwrap_or_default().contains("NeedsLogin"));
    }

    /// ADR 065 D4: undo drops the run's reference first; another setup
    /// still using the entry keeps it, and the worker is left alone.
    #[tokio::test]
    async fn undo_keeps_an_entry_another_setup_still_uses() {
        let ops = Fake {
            reachable: true,
            serve_status: Mutex::new(Some(applied())),
            serve_release: Some(ServeRelease::Kept(
                "homelab still serves it for another setup (set up on 3 October)".into(),
            )),
            ..Default::default()
        };
        let record = execute(&serve_input(), &ops, &Saved::default(), timing()).await;
        let mut journal = record.journal.clone();
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(outcome.results.iter().any(|r| r
            .label
            .starts_with("Kept homelab.example.ts.net on homelab")));
        assert!(ops
            .calls()
            .contains(&"release_serve_entry custom:50".into()));
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("forget_serve_entry")));
    }

    /// ADR 065 D4: the last reference has the agent take its handler off;
    /// undo waits for its report, then carries on with the older steps.
    #[tokio::test]
    async fn undo_waits_for_the_agent_to_remove_the_entry_then_carries_on() {
        let ops = Fake {
            reachable: true,
            serve_status: Mutex::new(Some(applied())),
            ..Default::default()
        };
        let record = execute(&serve_input(), &ops, &Saved::default(), timing()).await;
        let mut journal = record.journal.clone();
        let sink = Journaled::default();
        let outcome = undo(&mut journal, &ops, 0, &sink).await;
        let Some(Stop::Pending(why)) = &outcome.stopped else {
            panic!("expected to wait, got {:?}", outcome.stopped);
        };
        assert!(
            why.contains("homelab is taking homelab.example.ts.net off its Serve config"),
            "{why}"
        );
        // The domain this run attached is untouched while it waits.
        assert!(!ops.calls().iter().any(|c| c.starts_with("detach_domain")));
        assert!(journal.iter().any(|e| e.deleting));

        // Still removing: still waiting.
        let again = undo(&mut journal, &ops, 0, &sink).await;
        assert!(matches!(again.stopped, Some(Stop::Pending(_))));

        // The agent reported `removed`: the row is gone, undo carries on.
        *ops.serve_status.lock().expect("lock") = None;
        let done = undo(&mut journal, &ops, 0, &sink).await;
        assert_eq!(done.stopped, None, "{:?}", done.results);
        assert!(ops.calls().contains(&"detach_domain 50".into()));
    }

    /// ADR 065 D4: something else took the slot while the entry was being
    /// removed. Temps never removes it; keeping it forgets the entry.
    #[tokio::test]
    async fn undo_never_removes_a_handler_temps_did_not_write() {
        let ops = Fake {
            reachable: true,
            serve_status: Mutex::new(Some(applied())),
            ..Default::default()
        };
        let record = execute(&serve_input(), &ops, &Saved::default(), timing()).await;
        let mut journal = record.journal.clone();
        let sink = Journaled::default();
        let _ = undo(&mut journal, &ops, 0, &sink).await;
        *ops.serve_status.lock().expect("lock") = Some(ServeEntryStatus {
            removing: true,
            report: Some(REPORT_CONFLICT.into()),
            detail: Some("`http://127.0.0.1:3000`".into()),
            ..applied()
        });
        let outcome = undo(&mut journal, &ops, 0, &sink).await;
        let Some(Stop::Conflict(why)) = &outcome.stopped else {
            panic!("expected a conflict, got {:?}", outcome.stopped);
        };
        assert!(
            why.contains("never removes Serve configuration it did not write"),
            "{why}"
        );
        let key = journal
            .iter()
            .find(|e| matches!(e.intent, Intent::TailscaleServe { .. }))
            .map(|e| e.key.clone())
            .expect("entry");
        assert!(decide(&mut journal, &key, Decision::Overwrite, true).is_err());
        decide(&mut journal, &key, Decision::Keep, true).expect("kept");
        let outcome = undo(&mut journal, &ops, 0, &sink).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(ops.calls().contains(&"forget_serve_entry 70".into()));
        assert!(ops.calls().contains(&"detach_domain 50".into()));
    }

    /// A conflict report on the last reference means the agent wrote
    /// nothing: the row goes, undo does not wait.
    #[tokio::test]
    async fn undo_of_an_entry_the_agent_never_wrote_does_not_wait() {
        let ops = Fake {
            serve_status: Mutex::new(Some(ServeEntryStatus {
                entry_id: 70,
                report: Some(REPORT_CONFLICT.into()),
                ..ServeEntryStatus::default()
            })),
            serve_release: Some(ServeRelease::Deleted),
            ..Default::default()
        };
        let record = execute(&serve_input(), &ops, &Saved::default(), timing()).await;
        let mut journal = record.journal.clone();
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(ops.calls().contains(&"detach_domain 50".into()));
    }

    const INSTALL: &str = "inst1";

    fn service_input() -> RunInput {
        RunInput {
            option: PlanOptionKind::TailscaleService,
            address: "myapp.example.ts.net".into(),
            steps: steps(&[
                StepKind::AttachDomain,
                StepKind::SetAudience,
                StepKind::DefineTailscaleService,
                StepKind::ConfigureTailscaleServe,
                StepKind::WaitForServiceApproval,
                StepKind::CheckReachability,
            ]),
            audience: Some((
                AddressAudience {
                    audience: Audience::Private,
                    private_node_id: Some(2),
                    private_via: Some("tailscale_service".into()),
                    private_service: Some("svc:myapp".into()),
                    ..AddressAudience::default()
                },
                AddressAudience::public(),
            )),
            serve: Some(ServeTarget {
                node_id: 2,
                node_name: "homelab".into(),
                via: "tailscale_service".into(),
                service: Some("svc:myapp".into()),
                tailnet: Some("example.ts.net".into()),
            }),
            tailscale_service: Some(ServicePlan {
                name: "svc:myapp".into(),
                tailnet: "example.ts.net".into(),
                tag: "tag:temps".into(),
                install_id: INSTALL.into(),
                seen: None,
                credential_name: String::new(),
            }),
            ..serve_input()
        }
    }

    fn homelab_entry() -> ServiceEntryHost {
        ServiceEntryHost {
            node_id: 2,
            node_name: "homelab".into(),
            address_id: "custom:50".into(),
            hostname: "myapp.example.ts.net".into(),
            tailscale_node_id: Some("nHOMELAB".into()),
            dns_name: Some("homelab.example.ts.net".into()),
            removing: false,
        }
    }

    fn host(node_id: &str, name: &str, approved: Option<bool>) -> ServiceHost {
        ServiceHost {
            node_id: Some(node_id.into()),
            name: Some(name.into()),
            approved,
        }
    }

    /// A Service run whose worker is the approved host. Its Serve entry
    /// is released at once on undo, so undo reaches the Service.
    fn approved_service() -> Fake {
        Fake {
            serve_release: Some(ServeRelease::Gone),
            reachable: true,
            serve_status: Mutex::new(Some(applied())),
            service_entries: Mutex::new(vec![homelab_entry()]),
            service_hosts: Mutex::new(vec![host("nHOMELAB", "homelab.example.ts.net", Some(true))]),
            ..Default::default()
        }
    }

    fn service_resource(record: &RunRecord) -> Option<RunResource> {
        record
            .created()
            .into_iter()
            .find(|r| matches!(r, RunResource::TailscaleService { .. }))
    }

    /// ADR 065 D4: absent at the name, the run creates the fixed
    /// definition with its marker, journals what Tailscale assigned, and
    /// is done once the admin approved the worker.
    #[tokio::test]
    async fn a_service_run_creates_it_only_when_absent_and_waits_for_approval() {
        let ops = approved_service();
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);
        let marker = tailscale::marker(INSTALL, 0);
        assert!(ops.calls().contains(&format!("create_service {marker}")));
        match service_resource(&record) {
            Some(RunResource::TailscaleService {
                name,
                created,
                definition,
                ..
            }) => {
                assert_eq!(name, "svc:myapp");
                assert!(created);
                assert_eq!(definition.addrs, vec!["100.100.1.5".to_string()]);
                assert_eq!(definition.comment, marker);
                assert_eq!(definition.ports, vec!["tcp:443".to_string()]);
                assert_eq!(definition.tags, vec!["tag:temps".to_string()]);
            }
            other => panic!("no Service journaled: {other:?}"),
        }
        // The Serve entry names the Service, so the worker advertises it.
        assert!(ops
            .calls()
            .contains(&"claim_serve_entry homelab custom:50 myapp.example.ts.net".into()));

        // Not approved yet: waiting with the autoApprovers snippet, never
        // approving.
        let waiting = Fake {
            service_hosts: Mutex::new(vec![host("nHOMELAB", "homelab.example.ts.net", None)]),
            ..approved_service()
        };
        let record = execute(&service_input(), &waiting, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Waiting, "{:?}", record.error);
        assert_eq!(
            record.error.as_deref(),
            Some(
                temps_core::tailnet::messages::not_approved("svc:myapp", "homelab", "tag:temps")
                    .as_str()
            )
        );
    }

    /// ADR 065 D4: a Service Temps did not create is never written to:
    /// the run stops with nothing journaled for it.
    #[tokio::test]
    async fn a_foreign_service_at_the_name_is_never_written() {
        let ops = Fake {
            service: Mutex::new(Some(ServiceDefinition {
                name: "svc:myapp".into(),
                addrs: vec!["100.100.1.9".into()],
                comment: "the media server".into(),
                ports: vec!["tcp:443".into()],
                tags: vec!["tag:media".into()],
            })),
            ..approved_service()
        };
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Conflict);
        assert_eq!(
            record.error.as_deref(),
            Some(temps_core::tailnet::messages::service_taken("svc:myapp").as_str())
        );
        assert!(!ops.calls().iter().any(|c| c.starts_with("create_service")));
        assert!(service_resource(&record).is_none());
        assert!(!ops
            .calls()
            .iter()
            .any(|c| c.starts_with("claim_serve_entry")));

        // Another install's marker is foreign too.
        let other = Fake {
            service: Mutex::new(Some(ServiceDefinition::fixed(
                "svc:myapp",
                "tag:temps",
                &tailscale::marker("another-install", 4),
            ))),
            ..approved_service()
        };
        let record = execute(&service_input(), &other, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Conflict);
        assert!(!other
            .calls()
            .iter()
            .any(|c| c.starts_with("create_service")));
    }

    /// ADR 065 D4: a host no Serve entry accounts for is never accepted.
    #[tokio::test]
    async fn an_unrecorded_host_stops_the_run() {
        let ops = Fake {
            service_hosts: Mutex::new(vec![
                host("nHOMELAB", "homelab.example.ts.net", Some(true)),
                host("nNAS", "nas.example.ts.net", Some(true)),
            ]),
            ..approved_service()
        };
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Conflict);
        let error = record.error.unwrap_or_default();
        assert!(error.contains("`nas`"), "{error}");
        assert!(!error.contains("`homelab`"), "{error}");
    }

    /// A Service deleted after the define step is not recreated behind
    /// the user's back: the run stops so undo can take it apart.
    #[tokio::test]
    async fn a_service_deleted_after_it_was_defined_fails_the_run() {
        let ops = Fake {
            // The define step reads twice (absent, then the created one).
            vanish_after: Some(2),
            ..approved_service()
        };
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Failed, "{:?}", record.error);
        assert!(record.error.unwrap_or_default().contains("is gone"));
        assert_eq!(
            ops.calls()
                .iter()
                .filter(|c| c.starts_with("create_service"))
                .count(),
            1
        );
    }

    /// ADR 065 D4: a Service whose marker is no longer this run's (someone
    /// deleted Temps' and made their own at the name) is kept, and undo
    /// never offers to delete it, even with this run's creation journal.
    #[tokio::test]
    async fn undo_never_offers_to_delete_a_service_that_lost_this_runs_marker() {
        let ops = approved_service();
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);
        let created = ops.service.lock().expect("lock").clone().expect("created");
        for (case, comment) in [
            ("a foreign comment", "the media server".to_string()),
            ("no marker", String::new()),
            (
                "another run of this install",
                tailscale::marker(INSTALL, 99),
            ),
            ("another install", tailscale::marker("another-install", 0)),
        ] {
            let replaced = ServiceDefinition {
                comment: comment.clone(),
                ..created.clone()
            };
            let ops = Fake {
                creation: Some((created.clone(), chrono::Utc::now())),
                service: Mutex::new(Some(replaced)),
                serve_release: Some(ServeRelease::Gone),
                reachable: true,
                serve_status: Mutex::new(Some(applied())),
                ..Default::default()
            };
            let mut journal = record.journal.clone();
            let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
            assert_eq!(outcome.stopped, None, "{case}: {:?}", outcome);
            assert!(
                outcome
                    .results
                    .iter()
                    .any(|r| r.label.starts_with("Kept `svc:myapp`: `svc:myapp`")
                        && r.label.contains("not Temps' to delete")),
                "{case}: {:?}",
                outcome.results
            );
            assert!(
                !ops.calls().iter().any(|c| c.starts_with("delete_service")),
                "{case}"
            );
            assert!(
                journal.iter().all(|e| e.shown_service.is_none()),
                "{case}: nothing is shown to confirm"
            );
        }
    }

    #[test]
    fn only_this_runs_own_marker_is_temps_to_delete() {
        let ours =
            ServiceDefinition::fixed("svc:myapp", "tag:temps", &tailscale::marker(INSTALL, 7));
        assert!(still_ours(&ours, "svc:myapp", INSTALL, 7).is_ok());
        assert!(still_ours(&ours, "svc:myapp", INSTALL, 8).is_err());
        assert!(still_ours(&ours, "svc:myapp", "another-install", 7).is_err());
        // An unknown install owns nothing.
        assert!(still_ours(&ours, "svc:myapp", "", 7).is_err());
        let unmarked = ServiceDefinition {
            comment: String::new(),
            ..ours.clone()
        };
        assert!(still_ours(&unmarked, "svc:myapp", INSTALL, 7)
            .unwrap_err()
            .contains("no longer carries the marker"));
    }

    /// ADR 065 D4: undo keeps a Service any Serve entry still names, and
    /// never deletes one no journal of this install records creating.
    #[tokio::test]
    async fn undo_keeps_a_service_in_use_or_without_its_creation_journal() {
        let ops = approved_service();
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);

        // Still in use.
        let mut journal = record.journal.clone();
        *ops.service_entries.lock().expect("lock") = vec![ServiceEntryHost {
            node_name: "nas".into(),
            hostname: "myapp.example.ts.net".into(),
            ..homelab_entry()
        }];
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(outcome
            .results
            .iter()
            .any(|r| r.label == "Kept `svc:myapp`: it still serves myapp.example.ts.net on nas"));
        assert!(!ops.calls().iter().any(|c| c.starts_with("delete_service")));

        // Nothing uses it, but no journal records creating it.
        let mut journal = record.journal.clone();
        ops.service_entries.lock().expect("lock").clear();
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(outcome
            .results
            .iter()
            .any(|r| r.label
                == "Kept `svc:myapp`: no setup of this Temps install records creating it"));
        assert!(!ops.calls().iter().any(|c| c.starts_with("delete_service")));
    }

    /// ADR 065 D4: undo never deletes a Service on its own. It asks; a
    /// confirmed delete names what the user was shown, and a check that no
    /// longer holds under the lock keeps it. Going forward there is
    /// nothing to decide.
    #[tokio::test]
    async fn undo_deletes_a_service_only_once_a_person_confirms_it() {
        let ops = approved_service();
        let record = execute(&service_input(), &ops, &Saved::default(), timing()).await;
        assert_eq!(record.state, RunState::Succeeded, "{:?}", record.error);
        let shown = ops.service.lock().expect("lock").clone().expect("created");
        ops.service_entries.lock().expect("lock").clear();
        ops.service_hosts.lock().expect("lock").clear();
        let ops = Fake {
            creation: Some((shown.clone(), chrono::Utc::now())),
            service: Mutex::new(Some(shown.clone())),
            serve_release: Some(ServeRelease::Gone),
            reachable: true,
            serve_status: Mutex::new(Some(applied())),
            ..Default::default()
        };

        let mut journal = record.journal.clone();
        let outcome = undo(&mut journal, &ops, 0, &Journaled::default()).await;
        let Some(Stop::Conflict(question)) = outcome.stopped else {
            panic!("undo did not ask: {:?}", outcome);
        };
        assert!(
            question.starts_with(
                "Delete `svc:myapp` from tailnet `example.ts.net`? It was created by this \
                 Temps install on"
            ),
            "{question}"
        );
        assert!(!ops.calls().iter().any(|c| c.starts_with("delete_service")));
        let key = journal
            .iter()
            .find(|e| matches!(e.intent, Intent::TailscaleService { .. }))
            .map(|e| e.key.clone())
            .expect("the Service's entry");
        assert_eq!(
            journal
                .iter()
                .find(|e| e.key == key)
                .and_then(|e| e.shown_service.clone()),
            Some(shown.clone())
        );

        // Going forward is refused: Temps never changes a Service.
        let mut forward = journal.clone();
        assert!(decide(&mut forward, &key, Decision::Overwrite, false)
            .unwrap_err()
            .contains("never changes a Tailscale Service"));

        // Confirmed: deleted, naming what was shown.
        let mut confirmed = journal.clone();
        decide(&mut confirmed, &key, Decision::Overwrite, true).expect("decide");
        let outcome = undo(&mut confirmed, &ops, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(ops.calls().contains(&format!(
            "delete_service example.ts.net svc:myapp {}",
            shown.comment
        )));
        assert!(outcome
            .results
            .iter()
            .any(|r| r.label == "Deleted `svc:myapp` from example.ts.net"));

        // A check that no longer holds under the lock keeps it.
        let reused = Fake {
            deletion: Some(ServiceDeletion::Kept(
                "`svc:myapp` is used again, by `myapp` on `nas`".into(),
            )),
            serve_release: Some(ServeRelease::Gone),
            ..Default::default()
        };
        let mut confirmed = journal.clone();
        decide(&mut confirmed, &key, Decision::Overwrite, true).expect("decide");
        let outcome = undo(&mut confirmed, &reused, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(
            outcome
                .results
                .iter()
                .any(|r| r.label
                    == "Kept `svc:myapp`: `svc:myapp` is used again, by `myapp` on `nas`")
        );

        // Declined: kept, nothing deleted.
        let declined = Fake::default();
        let mut kept = journal.clone();
        decide(&mut kept, &key, Decision::Keep, true).expect("decide");
        let outcome = undo(&mut kept, &declined, 0, &Journaled::default()).await;
        assert_eq!(outcome.stopped, None, "{:?}", outcome.results);
        assert!(!declined
            .calls()
            .iter()
            .any(|c| c.starts_with("delete_service")));
    }
}
