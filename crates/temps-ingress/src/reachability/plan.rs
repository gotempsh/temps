// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What *Make it reachable* would do for an address, from what the cluster
//! already has (ADR 064 D4). Pure: the service gathers the facts.
//!
//! For the public audience, in the order Temps prefers them:
//!
//! | What the cluster has | Option | Actions |
//! |---|---|---|
//! | A tunnel that would publish the hostname | `existing_tunnel` | 1 |
//! | A tunnel that covers it but would not publish it | `existing_tunnel`, confirmed | 2 |
//! | A direct ingress on port 80 and a zone Temps manages | `direct_managed_dns` | 1 |
//! | A Cloudflare account, no covering tunnel | `new_tunnel` | 2 |
//! | Nothing | `new_tunnel_with_token` | 3 |
//! | A direct ingress on port 80, DNS elsewhere | `direct_manual_dns` | 2 |
//!
//! Direct options issue the certificate with HTTP-01, so they are only
//! offered when a direct ingress answers on port 80.
//!
//! For the private audience (ADR 064 D5) there is one option,
//! `private_worker`: a worker whose owner put it in `private` mode serves
//! the address on its private listener, DNS in a zone Temps manages points
//! at that listener, and the certificate comes over DNS-01. Temps cannot
//! turn private mode on itself; without such a worker the plan names the
//! command and the worker to run it on.
//!
//! A worker whose owner also allowed Tailscale Serve (ADR 065 D1) offers
//! `tailscale_serve` instead: the worker's own tailnet name, served by
//! tailscaled, with no DNS record and no ACME. It and the listener are
//! exclusive on one worker, refused both ways.
//!
//! Any other name under the worker's tailnet is a Tailscale Service
//! (`tailscale_service`): the app's own name, which needs the cluster's
//! Tailscale OAuth client (`tailscale_service_with_credential` pastes one)
//! and a tagged worker. With nothing asked for, the plan suggests, in
//! ADR 065 D1's order: a Service when a usable credential exists, the
//! machine's name, an address under a zone for the listener, then a Service
//! after pasting a credential.
//!
//! "Would publish" is ADR 058 D9's effective publication state
//! ([`PublicationIntent`]), not whether a tunnel's domain covers the
//! hostname: a hostname choice wins over automatic selection, and a tunnel
//! with `auto_publish` off publishes only the hostnames chosen for it. An
//! option that changes a hostname's choice says so in `confirmation`.
//!
//! [`fingerprint`] hashes everything a plan depends on, so `apply` can tell
//! a plan the cluster has moved on from: that includes the current content
//! of every DNS record the plan would write (provider, record id, type,
//! content, modification time), read when planning ([`records_to_read`]).

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use temps_core::tailnet::{messages as tailnet_messages, TailscaleReport};
use temps_core::ways_in::{private_allow_list_message, PrivateAllowList};

use temps_auth::permissions::Permission;
use temps_core::gate::GateKind;

use super::access::{AccessApp, AccessPolicyAnswer};
use super::compute::{
    is_local_only, tailnet_worker_problem, DirectIngress, INGRESS_SETUP_PATH, PRIVATE_COMMAND,
    TAILNET_REPORT_STALE_MINUTES,
};
use super::edge_rules::{below_free_certificate, EdgeRulePurpose, HttpPolicy};
use super::model::{
    ActionKind, AddressAudience, Audience, AudienceChoice, PlanBlocked, PlanOption, PlanOptionKind,
    PlanStep, PlannedRecord, ReachAction, ReachabilityPlan, StepKind, WayInChoice,
};
use crate::registry::{FieldChoice, FieldKind, SetupField};
use crate::service::{covers, normalize_host, record_label, PublicationIntent, TunnelFact};
use crate::tailscale::{self, Ownership, ServiceDefinition, TailscaleFactory};

/// Field keys of the apply request.
pub const FIELD_ADDRESS: &str = "address";
pub const FIELD_DNS_PROVIDER: &str = "dns_provider_id";
pub const FIELD_TOKEN: &str = "cloudflare_api_token";
/// What a tunnel address answers besides HTTPS (ADR 070): an
/// [`HttpPolicy`] value.
pub const FIELD_HTTP_POLICY: &str = "http_policy";
/// The teams that may sign in to a signed-in address (ADR 065 D7).
pub const FIELD_GATE_TEAMS: &str = "gate_teams";
/// The users that may sign in to a signed-in address (ADR 065 D7).
pub const FIELD_GATE_USERS: &str = "gate_users";
/// The Cloudflare Access token whose account gets the app (ADR 065 D9).
pub const FIELD_ACCESS_PROVIDER: &str = "access_provider_id";
/// Who a new Access app lets in: email addresses, and email domains.
pub const FIELD_ACCESS_EMAILS: &str = "access_emails";
pub const FIELD_ACCESS_EMAIL_DOMAINS: &str = "access_email_domains";
/// Where a Cloudflare Access token is added.
pub const ACCESS_SETUP_PATH: &str = "/dns-providers#cloudflare-access";

/// A saved Cloudflare Access token, and the apps in its account that
/// protect the planned address (ADR 065 D9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessProviderFact {
    pub id: i32,
    pub name: String,
    /// Its apps covering the address, ranked by [`super::access::covering`]
    /// (the one Cloudflare enforces first); why they could not be read.
    pub covering: Result<Vec<AccessApp>, String>,
}

/// What a Cloudflare Access plan does about the app in front of an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessChoice<'a> {
    /// No Access token is saved.
    NoProvider,
    /// An account's apps could not be read, so Temps cannot tell whether
    /// one already covers the address.
    Unreadable {
        provider: &'a AccessProviderFact,
        error: &'a str,
    },
    /// An existing app covers it: used as found, never edited or deleted.
    Existing {
        provider: &'a AccessProviderFact,
        app: &'a AccessApp,
    },
    /// None does: the run creates one in one of these accounts.
    Create(Vec<&'a AccessProviderFact>),
}

/// The app a Cloudflare Access plan for an address uses or creates.
pub fn access_choice(facts: &PlanFacts) -> AccessChoice<'_> {
    if facts.access_providers.is_empty() {
        return AccessChoice::NoProvider;
    }
    for provider in &facts.access_providers {
        match &provider.covering {
            Err(error) => return AccessChoice::Unreadable { provider, error },
            Ok(apps) => {
                // `covering` comes ranked: the app Cloudflare enforces first.
                if let Some(app) = apps.first() {
                    return AccessChoice::Existing { provider, app };
                }
            }
        }
    }
    AccessChoice::Create(facts.access_providers.iter().collect())
}

/// A zone a DNS provider of the cluster manages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneFact {
    pub provider_id: i32,
    pub provider_name: String,
    pub provider_type: String,
    pub domain: String,
    pub verified: bool,
    pub auto_manage: bool,
}

/// A worker in `private` mode (ADR 064 D5), as it reports itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrivateWorkerFact {
    pub id: i32,
    pub name: String,
    /// The private listener's address, when it is bound on a private one.
    pub listener: Option<std::net::IpAddr>,
    /// That address is the worker's Tailscale one.
    pub tailscale: bool,
    /// Why the listener is not running, as the worker reported it.
    pub error: Option<String>,
}

/// A worker's tailscaled as its agent last reported it (ADR 065 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetWorkerFact {
    pub id: i32,
    pub name: String,
    pub report: TailscaleReport,
    /// How old the report is.
    pub minutes_ago: i64,
    /// Its phase 2 private listener, when that is bound on its Tailscale
    /// address: Serve on 443 there would take its traffic.
    pub listener_on_tailscale: Option<std::net::IpAddr>,
}

/// A Serve entry the control plane declared for a worker (ADR 065 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeEntryFact {
    pub node_id: i32,
    pub hostname: String,
    /// `tailscale_serve` or `tailscale_service`.
    pub via: String,
    pub service: Option<String>,
    pub removing: bool,
}

/// The cluster's Tailscale OAuth client (ADR 065 D2), as planning found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleCredentialFact {
    pub id: i32,
    pub name: String,
    /// Why a run cannot use it (a scope missing or unproven), when it
    /// cannot: the Service way in is then unavailable.
    pub refusal: Option<String>,
}

/// What the Tailscale API holds at the Service name a plan is for, read
/// when planning (ADR 065 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceSeen {
    Absent,
    Found(ServiceDefinition),
    /// The read failed: nothing is offered on a guess.
    Unreadable(String),
}

impl PrivateWorkerFact {
    pub fn ready(&self) -> Option<std::net::IpAddr> {
        self.listener.filter(|_| self.error.is_none())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NamedId {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct PlanFacts {
    pub audience: Audience,
    pub project_slug: String,
    pub environment_slug: String,
    pub environment_name: String,
    pub deployed: bool,
    /// The address asked for, as given.
    pub requested: Option<String>,
    /// Addresses already on this environment.
    pub attached_here: Vec<String>,
    /// Custom domains of every other environment.
    pub taken_elsewhere: HashSet<String>,
    pub tunnels: Vec<TunnelFact>,
    pub zones: Vec<ZoneFact>,
    /// Active Cloudflare DNS providers.
    pub cloudflare_accounts: Vec<NamedId>,
    pub directs: Vec<DirectIngress>,
    /// Active workers that let Temps run a tunnel on them.
    pub tunnel_workers: Vec<NamedId>,
    /// Active workers whose owner keeps Temps from exposing them.
    pub workers_refusing: Vec<String>,
    /// Whether workers serve the environment, so a tunnel can carry it.
    pub served_by_workers: bool,
    /// Which tunnel would publish each hostname asked about (ADR 058 D9).
    pub intents: HashMap<String, PublicationIntent>,
    /// An IP allow list of only private ranges governing the environment,
    /// which no visitor through a tunnel can match (ADR 064 D9).
    pub private_rule: Option<PrivateAllowList>,
    /// The environment's addresses and their audiences.
    pub audiences: Vec<(String, Audience)>,
    /// Public hostnames that reach one Compose service of the environment
    /// (generated `<service>--<env>` names, service custom domains). They
    /// open the same app, and a signed-in plan says so (ADR 065 D8).
    pub service_addresses: Vec<String>,
    /// Every hostname choice.
    pub choices: Vec<(String, Option<i32>)>,
    /// Active workers and their ingress mode.
    pub worker_modes: Vec<(i32, Option<String>)>,
    /// What is at each `(hostname, record type)` the plan would write, as
    /// read when planning ([`records_to_read`]).
    pub dns_records: Vec<(String, String, PlannedRecord)>,
    /// Workers in `private` mode.
    pub private_workers: Vec<PrivateWorkerFact>,
    /// The worker to name when none is in private mode: one the
    /// environment runs on, else the first active worker.
    pub suggested_private_worker: Option<String>,
    /// The environment's private addresses and the range each is bound to.
    pub private_ranges: Vec<(String, Option<String>)>,
    /// The label a tunnel's environment hosts give this environment
    /// (ADR 068 D3): its subdomain, or the production template's.
    pub environment_host_label: Option<String>,
    /// Whether the environment or its project has password protection on,
    /// which applies on top of a sign-in (ADR 065 D7).
    pub password_protected: bool,
    /// For `signed_in`: the gate asked for.
    pub gate: GateKind,
    /// For Cloudflare Access: the saved tokens and the apps covering the
    /// planned address. Read only for such a plan.
    pub access_providers: Vec<AccessProviderFact>,
    /// Workers that report their tailscaled (ADR 065 D2).
    pub tailnet_workers: Vec<TailnetWorkerFact>,
    /// The Serve entries declared on those workers.
    pub serve_entries: Vec<ServeEntryFact>,
    /// The cluster's Tailscale OAuth client, when it has one (ADR 065 D2).
    pub tailscale_credential: Option<TailscaleCredentialFact>,
    /// The Service name the plan is for (`svc:myapp`) and what the API
    /// holds there, read once the address is known.
    pub service_seen: Option<(String, ServiceSeen)>,
    /// ADR 031's install id: a Service whose marker carries it is Temps'.
    pub install_id: String,
    /// For `tailnet_users`: who the address is to admit, normalized (ADR
    /// 065 D6).
    pub principals: Vec<temps_core::tailnet::TailnetPrincipal>,
    /// Who the requested address admits now, when it is `tailnet_users`.
    pub principals_now: Vec<temps_core::tailnet::TailnetPrincipal>,
    /// Workers whose agent reports `tailnet_identity.v1`: the only ones a
    /// `tailnet_users` address is sent to.
    pub identity_workers: Vec<String>,
    /// The recorded HTTPS-only choice of each attached address that has
    /// one (ADR 070).
    pub http_policies: HashMap<String, HttpPolicy>,
}

/// The worker a private plan serves through: the first whose private
/// listener is bound on a private address.
pub fn private_worker(facts: &PlanFacts) -> Option<(&PrivateWorkerFact, std::net::IpAddr)> {
    facts
        .private_workers
        .iter()
        .find_map(|w| w.ready().map(|ip| (w, ip)))
}

/// The `A`/`AAAA` record type for an address.
pub fn record_type_of(ip: std::net::IpAddr) -> &'static str {
    match ip {
        std::net::IpAddr::V4(_) => "A",
        std::net::IpAddr::V6(_) => "AAAA",
    }
}

/// The records a plan for `address` could write through a zone Temps
/// manages: the service reads them before planning, so the plan shows
/// what would change and its fingerprint holds them.
pub fn records_to_read(facts: &PlanFacts, address: &str) -> Vec<(String, String)> {
    if !zone_for(address, &facts.zones).is_some_and(|z| z.verified) {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    if facts.audience.is_private() {
        if let Some((_, ip)) = private_worker(facts) {
            out.push((address.to_string(), record_type_of(ip).to_string()));
        }
    } else if let Some(direct) = http01_direct(facts) {
        out.push((
            address.to_string(),
            record_type_of(direct.ips[0]).to_string(),
        ));
    }
    out.sort();
    out.dedup();
    out
}

/// What the plan saw at `host`'s `record_type` record.
pub fn planned_record(facts: &PlanFacts, host: &str, record_type: &str) -> PlannedRecord {
    facts
        .dns_records
        .iter()
        .find(|(h, t, _)| h == host && t == record_type)
        .map(|(_, _, r)| r.clone())
        .unwrap_or_default()
}

/// The address a plan is for: as asked, else a suggestion. `Err` with why
/// the address asked for cannot be used.
pub fn planned_address(facts: &PlanFacts) -> Result<Option<String>, String> {
    match facts.requested.as_deref().map(str::trim) {
        Some(requested) if !requested.is_empty() => match normalize_host(requested) {
            Err(error) => Err(error.to_string()),
            Ok(host) if is_local_only(&host) => Err(format!(
                "{host} only resolves to the computer it is opened on. Use a hostname on a \
                 domain you own."
            )),
            Ok(host) if facts.taken_elsewhere.contains(&host) => Err(format!(
                "{host} is already attached to another environment. Remove it there first, or \
                 use another hostname."
            )),
            Ok(host) => Ok(Some(host)),
        },
        _ => Ok(suggest_address(facts)),
    }
}

/// The `(id, aud)` of the Access apps an account has for the address, in
/// the fingerprint; `None` stands for an account that could not be read.
type AppIds<'a> = Vec<(&'a str, &'a str)>;

/// The address the plan for `facts.audience` is for: what the stored plan
/// records and the fingerprint covers.
pub fn address_for_audience(facts: &PlanFacts) -> Result<Option<String>, String> {
    if facts.audience.is_private() {
        planned_private_address(facts)
    } else {
        planned_address(facts)
    }
}

/// A hash of everything a plan for `address` depends on (ADR 064 D4).
pub fn fingerprint(facts: &PlanFacts, address: Option<&str>) -> String {
    let mut tunnels = facts.tunnels.clone();
    tunnels.sort_by_key(|t| t.id);
    let mut zones = facts.zones.clone();
    zones.sort_by(|a, b| (a.provider_id, &a.domain).cmp(&(b.provider_id, &b.domain)));
    let mut accounts: Vec<i32> = facts.cloudflare_accounts.iter().map(|a| a.id).collect();
    accounts.sort_unstable();
    let mut audiences = facts.audiences.clone();
    audiences.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
    let mut choices = facts.choices.clone();
    choices.sort();
    let mut modes = facts.worker_modes.clone();
    modes.sort();
    let mut directs: Vec<_> = facts
        .directs
        .iter()
        .map(|d| {
            (
                &d.name,
                d.node_id,
                &d.ips,
                d.takes_port_80,
                d.check_failed.is_some(),
            )
        })
        .collect();
    directs.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let intent = address.map(|a| intent_of(facts, a));
    let mut records = facts.dns_records.clone();
    records.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut private_workers: Vec<_> = facts
        .private_workers
        .iter()
        .map(|w| (w.id, w.listener, w.tailscale, w.error.is_some()))
        .collect();
    private_workers.sort();
    let mut ranges = facts.private_ranges.clone();
    ranges.sort();
    // An app created or removed in Cloudflare since planning changes what
    // an Access plan does.
    let mut access: Vec<(i32, Option<AppIds<'_>>)> = facts
        .access_providers
        .iter()
        .map(|p| {
            let apps = p.covering.as_ref().ok().map(|apps| {
                let mut ids: Vec<(&str, &str)> = apps
                    .iter()
                    .map(|a| (a.id.as_str(), a.aud.as_str()))
                    .collect();
                ids.sort();
                ids
            });
            (p.id, apps)
        })
        .collect();
    access.sort_by_key(|(id, _)| *id);
    let gate = (facts.audience == Audience::SignedIn).then_some(facts.gate);
    // ADR 065 D4: the Tailscale facts as last reported, without the inside
    // checks and entry outcomes, which change on every pass.
    let mut tailnet: Vec<_> = facts
        .tailnet_workers
        .iter()
        .map(|w| {
            let r = &w.report;
            // Tuples over twelve fields have no `Debug`: two halves.
            (
                (
                    w.id,
                    r.consent,
                    r.root,
                    r.state,
                    r.version.clone(),
                    r.backend_state.clone(),
                    r.tags.clone(),
                ),
                (
                    r.magic_dns,
                    r.https(),
                    r.dns_name.clone(),
                    r.port_443_held_by.clone(),
                    r.port_443_funnel,
                    w.minutes_ago > TAILNET_REPORT_STALE_MINUTES,
                    w.listener_on_tailscale,
                ),
            )
        })
        .collect();
    tailnet.sort_by_key(|t| t.0 .0);
    let mut entries: Vec<_> = facts
        .serve_entries
        .iter()
        .map(|e| (e.node_id, &e.hostname, &e.via, &e.service, e.removing))
        .collect();
    entries.sort();
    // ADR 065 D4: the credential, and the Service as read when planning.
    let credential = facts
        .tailscale_credential
        .as_ref()
        .map(|c| (c.id, c.refusal.is_some()));
    let seen = &facts.service_seen;
    let canonical = format!(
        "v5|{:?}|{address:?}|{audiences:?}|{}|{}|{}|{tunnels:?}|{choices:?}|{zones:?}|\
         {accounts:?}|{directs:?}|{modes:?}|{intent:?}|{:?}|{records:?}|{private_workers:?}|\
         {ranges:?}|{tailnet:?}|{entries:?}|{credential:?}|{seen:?}|{:?}{}",
        facts.audience,
        facts.deployed,
        facts.served_by_workers,
        address.is_some_and(|a| facts.taken_elsewhere.contains(a)),
        facts.private_rule,
        facts.environment_host_label,
        // Only signed-in plans gain a part, so other plans hash as before.
        gate.map(|gate| format!("|{gate:?}|{access:?}"))
            .unwrap_or_default(),
    );
    // ADR 065 D6: who it admits, asked for and now, and which agents can
    // check it. Only for `tailnet_users`, so other plans hash as before.
    let canonical = if facts.audience == Audience::TailnetUsers {
        let mut identity = facts.identity_workers.clone();
        identity.sort();
        format!(
            "{canonical}|{:?}|{:?}|{identity:?}",
            facts.principals, facts.principals_now
        )
    } else {
        canonical
    };
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

/// The tunnel an address goes through, and whether its hostname choice has
/// to be set for that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelRoute<'a> {
    pub tunnel: &'a TunnelFact,
    /// A `RouteHostname` step records the choice.
    pub route: bool,
    /// Set with `route`: what the choice changes, for the user to confirm.
    pub confirmation: Option<String>,
}

fn usable(tunnel: &TunnelFact) -> bool {
    tunnel.ready && tunnel.has_nodes
}

fn unusable_because(tunnel: &TunnelFact) -> &'static str {
    if tunnel.ready {
        "runs on no worker"
    } else {
        "is not ready"
    }
}

/// Which tunnel carries `host`: the one that would publish it, else the
/// most specific covering one with its choice recorded and confirmed.
pub fn tunnel_route<'a>(facts: &'a PlanFacts, host: &str) -> Option<TunnelRoute<'a>> {
    let intent = Some(intent_of(facts, host));
    let find = |id: i32| facts.tunnels.iter().find(|t| t.id == id);
    if let Some(PublicationIntent::Tunnel { connector_id, .. }) = intent {
        if let Some(tunnel) = find(connector_id).filter(|t| usable(t)) {
            return Some(TunnelRoute {
                tunnel,
                route: false,
                confirmation: None,
            });
        }
    }
    let tunnel = facts
        .tunnels
        .iter()
        .filter(|t| usable(t))
        .filter(|t| {
            t.zone.as_deref().is_some_and(|zone| {
                covers(&t.domain, zone, host) && record_label(host, &t.domain, zone).is_ok()
            })
        })
        .max_by_key(|t| t.domain.len())?;
    let route = format!("Route `{host}` through tunnel `{}`", tunnel.name);
    let why = match intent {
        Some(PublicationIntent::ChosenOff) => "it is set to no tunnel now".to_string(),
        Some(PublicationIntent::ChosenMissing) => {
            "the tunnel chosen for it no longer exists".to_string()
        }
        Some(PublicationIntent::Tunnel {
            connector_id,
            by_choice,
        }) => match find(connector_id) {
            Some(other) if by_choice => format!(
                "it is set to tunnel `{}` now, which {}",
                other.name,
                unusable_because(other)
            ),
            Some(other) => format!(
                "tunnel `{}` would publish it, but {}",
                other.name,
                unusable_because(other)
            ),
            None => "the tunnel that would publish it is gone".to_string(),
        },
        Some(PublicationIntent::OnlyChosen { .. }) | Some(PublicationIntent::NoTunnel) | None => {
            format!(
                "tunnel `{}` publishes only hostnames chosen for it",
                tunnel.name
            )
        }
    };
    Some(TunnelRoute {
        tunnel,
        route: true,
        confirmation: Some(format!("{route}: {why}.")),
    })
}

/// ADR 058 D9's [`PublicationIntent`] for `host`: as the ingress service
/// worked it out, else the same selection over `facts` (a choice wins, then
/// the most specific automatic tunnel, the oldest on a tie).
pub fn intent_of(facts: &PlanFacts, host: &str) -> PublicationIntent {
    if let Some(intent) = facts.intents.get(host) {
        return *intent;
    }
    if let Some((_, choice)) = facts.choices.iter().find(|(h, _)| h == host) {
        return match choice {
            None => PublicationIntent::ChosenOff,
            Some(id) if facts.tunnels.iter().any(|t| t.id == *id) => PublicationIntent::Tunnel {
                connector_id: *id,
                by_choice: true,
            },
            Some(_) => PublicationIntent::ChosenMissing,
        };
    }
    let candidates: Vec<&TunnelFact> = facts
        .tunnels
        .iter()
        .filter(|t| {
            t.zone
                .as_deref()
                .is_some_and(|zone| covers(&t.domain, zone, host))
        })
        .collect();
    let automatic = candidates
        .iter()
        .filter(|t| t.auto_publish)
        .max_by(|a, b| a.domain.len().cmp(&b.domain.len()).then(b.id.cmp(&a.id)));
    match (automatic, candidates.first()) {
        (Some(t), _) => PublicationIntent::Tunnel {
            connector_id: t.id,
            by_choice: false,
        },
        (None, Some(t)) => PublicationIntent::OnlyChosen { connector_id: t.id },
        (None, None) => PublicationIntent::NoTunnel,
    }
}

/// For a new tunnel: what routing `host` through it changes, when the
/// hostname has a choice already.
pub fn new_tunnel_confirmation(facts: &PlanFacts, host: &str) -> Option<String> {
    match Some(intent_of(facts, host)) {
        Some(PublicationIntent::ChosenOff) => Some(format!(
            "Route `{host}` through the new tunnel: it is set to no tunnel now."
        )),
        Some(PublicationIntent::ChosenMissing) => Some(format!(
            "Route `{host}` through the new tunnel: the tunnel chosen for it no longer exists."
        )),
        Some(PublicationIntent::Tunnel {
            by_choice: true, ..
        }) => Some(format!(
            "Route `{host}` through the new tunnel instead of the one chosen for it."
        )),
        _ => None,
    }
}

/// The managed zone an address is in: the most specific one.
pub fn zone_for<'a>(address: &str, zones: &'a [ZoneFact]) -> Option<&'a ZoneFact> {
    zones
        .iter()
        .filter(|z| {
            let zone = z.domain.trim_end_matches('.').to_ascii_lowercase();
            address == zone || address.ends_with(&format!(".{zone}"))
        })
        .max_by_key(|z| z.domain.len())
}

/// The zone for an address no provider manages yet: its last two labels.
/// Verifying the domain with the provider tells when that is wrong.
pub fn guess_zone(address: &str) -> String {
    let labels: Vec<&str> = address.split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

/// The domain a new tunnel takes: the zone when the address is right under
/// it (Cloudflare's free certificate covers `*.zone`), else the address's
/// parent.
pub fn tunnel_domain_for(address: &str, zone: &str) -> String {
    match address.strip_suffix(&format!(".{zone}")) {
        Some(label) if !label.contains('.') => zone.to_string(),
        _ => address
            .split_once('.')
            .map(|(_, parent)| parent.to_string())
            .unwrap_or_else(|| zone.to_string()),
    }
}

/// A tunnel name from its domain that no other tunnel has.
pub fn unique_tunnel_name(domain: &str, taken: &[String]) -> String {
    let mut base: String = domain
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    base.truncate(40);
    let base = if base.is_empty() {
        "tunnel".to_string()
    } else {
        base
    };
    let taken: HashSet<String> = taken.iter().map(|n| n.to_ascii_lowercase()).collect();
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken.contains(name))
        .unwrap_or(base)
}

/// The first label of a suggested address.
fn suggested_label(facts: &PlanFacts) -> String {
    let project = facts.project_slug.to_ascii_lowercase();
    let env = facts.environment_slug.to_ascii_lowercase();
    if matches!(env.as_str(), "production" | "prod" | "main" | "") {
        project
    } else {
        format!("{project}-{env}")
    }
}

/// An address under a domain the cluster already has, preferring a tunnel's,
/// that nothing uses yet.
pub fn suggest_address(facts: &PlanFacts) -> Option<String> {
    let label = suggested_label(facts);
    let usable_tunnels = facts
        .tunnels
        .iter()
        .filter(|t| t.ready && t.has_nodes && t.zone.is_some())
        .map(|t| t.domain.clone());
    let zones = facts
        .zones
        .iter()
        .filter(|z| z.verified)
        .map(|z| z.domain.trim_end_matches('.').to_ascii_lowercase());
    usable_tunnels
        .chain(zones)
        .map(|domain| format!("{label}.{domain}"))
        .find(|candidate| {
            !facts.taken_elsewhere.contains(candidate) && !facts.attached_here.contains(candidate)
        })
}

/// What applying an option needs on top of `projects:write`.
pub fn permissions_for(kind: PlanOptionKind) -> Vec<Permission> {
    match kind {
        PlanOptionKind::ExistingTunnel => vec![Permission::SettingsWrite],
        PlanOptionKind::DirectManagedDns => {
            vec![Permission::DnsProvidersWrite, Permission::DomainsCreate]
        }
        PlanOptionKind::NewTunnel | PlanOptionKind::NewTunnelWithToken => {
            vec![Permission::SettingsWrite, Permission::DnsProvidersWrite]
        }
        PlanOptionKind::DirectManualDns => vec![Permission::DomainsCreate],
        PlanOptionKind::PrivateWorker => {
            vec![Permission::DnsProvidersWrite, Permission::DomainsCreate]
        }
        // No DNS record, no certificate: the address row only.
        PlanOptionKind::TailscaleServe | PlanOptionKind::TailscaleService => {
            vec![Permission::DomainsCreate]
        }
        // Saving the OAuth client is a cluster-wide setting.
        PlanOptionKind::TailscaleServiceWithCredential => {
            vec![Permission::SettingsWrite, Permission::DomainsCreate]
        }
        PlanOptionKind::TunnelEnvironmentHosts => vec![Permission::SettingsWrite],
    }
}

/// What the opt-in to a tunnel's environment hosts says first (ADR 068).
pub const ENVIRONMENT_HOSTS_WARNING: &str = "Every environment, including preview environments \
     named after branches, gets a public DNS name under the tunnel's domain: branch names \
     become public, and each name's certificate appears in Certificate Transparency logs.";

/// The tunnel whose environment hosts would give this environment `host`
/// (ADR 068 D3): `host` is `<its label>.<tunnel domain>`, the tunnel can
/// publish, its environment hosts are off, and no other tunnel's are on.
pub fn environment_hosts_tunnel<'a>(facts: &'a PlanFacts, host: &str) -> Option<&'a TunnelFact> {
    let label = facts.environment_host_label.as_deref()?;
    if facts.tunnels.iter().any(|t| t.environment_hosts) {
        return None;
    }
    // Hosts are never generated for an environment with a restricted
    // address (ADR 068 Security), nor offered as the way to make one: they
    // are public, and a signed-in plan would publish them without a gate.
    if facts.audience != Audience::Public
        || facts.audiences.iter().any(|(_, a)| *a != Audience::Public)
    {
        return None;
    }
    facts.tunnels.iter().find(|t| {
        usable(t)
            && t.zone.is_some()
            && host
                == format!(
                    "{label}.{}",
                    t.domain.trim_end_matches('.').to_ascii_lowercase()
                )
    })
}

fn address_field(suggested: Option<&str>) -> SetupField {
    SetupField {
        key: FIELD_ADDRESS.to_string(),
        label: "Address".to_string(),
        help: Some("The hostname people will open, on a domain you own.".to_string()),
        kind: FieldKind::Text,
        required: true,
        placeholder: Some(suggested.unwrap_or("myapp.example.com").to_string()),
    }
}

/// The Cloudflare token template link (ADR 064 D8): Zone · Zone · Read and
/// Zone · DNS · Edit filled in. Cloudflare documents no template key for
/// Account · Cloudflare Tunnel · Edit, so the user adds that one by hand.
pub fn cloudflare_token_url() -> String {
    // [{"key":"zone","type":"read"},{"key":"dns","type":"edit"}], URL-encoded.
    "https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=\
     %5B%7B%22key%22%3A%22zone%22%2C%22type%22%3A%22read%22%7D%2C%7B%22key%22%3A%22dns%22%2C\
     %22type%22%3A%22edit%22%7D%5D&accountId=*&zoneId=all&name=Temps"
        .to_string()
}

fn token_field() -> SetupField {
    SetupField {
        key: FIELD_TOKEN.to_string(),
        label: "Cloudflare API token".to_string(),
        help: Some(format!(
            "Create one at {} with Zone · Zone · Read and Zone · DNS · Edit filled in; add \
             Account · Cloudflare Tunnel · Edit yourself, and limit Zone Resources to this \
             address's zone only. For HTTPS only on standard ports, also add {} and {} \
             (optional: without them the address is still published). Temps checks it before \
             creating anything and stores it encrypted.",
            cloudflare_token_url(),
            EdgeRulePurpose::HttpsRedirect.permission(),
            EdgeRulePurpose::StandardPorts.permission(),
        )),
        kind: FieldKind::Secret,
        required: true,
        placeholder: None,
    }
}

fn provider_field() -> SetupField {
    SetupField {
        key: FIELD_DNS_PROVIDER.to_string(),
        label: "Cloudflare account".to_string(),
        help: Some("The account whose token creates the tunnel and its DNS record.".to_string()),
        kind: FieldKind::DnsProvider {
            provider_type: Some("cloudflare".to_string()),
        },
        required: true,
        placeholder: None,
    }
}

fn step(kind: StepKind, label: impl Into<String>) -> PlanStep {
    PlanStep {
        kind,
        label: label.into(),
    }
}

struct OptionDraft {
    kind: PlanOptionKind,
    title: String,
    description: String,
    actions: u8,
    way_in: String,
    steps: Vec<PlanStep>,
    fields: Vec<SetupField>,
    warnings: Vec<String>,
    confirmation: Option<String>,
}

/// The tunnel whose domain covers `host` most specifically.
pub fn covering_tunnel<'a>(facts: &'a PlanFacts, host: &str) -> Option<&'a TunnelFact> {
    facts
        .tunnels
        .iter()
        .filter(|t| {
            t.zone.as_deref().is_some_and(|zone| {
                covers(&t.domain, zone, host) && record_label(host, &t.domain, zone).is_ok()
            })
        })
        .max_by_key(|t| t.domain.len())
}

/// The direct ingress a certificate through HTTP-01 can be issued on: it
/// answers on port 80 and passed its last check.
pub fn http01_direct(facts: &PlanFacts) -> Option<&DirectIngress> {
    facts
        .directs
        .iter()
        .find(|d| d.takes_port_80 && d.check_failed.is_none() && !d.ips.is_empty())
}

/// Why no tunnel can carry the environment: it is deployed and its workers
/// serve none of it, so it runs on the control plane.
fn tunnel_unserved(facts: &PlanFacts) -> Option<PlanBlocked> {
    (facts.deployed && !facts.served_by_workers).then(|| PlanBlocked {
        reason: format!(
            "{} runs on the control plane, and a tunnel only carries what workers serve. \
             Deploy it to a worker to put it behind a tunnel.",
            facts.environment_name
        ),
        action: None,
    })
}

/// Why no tunnel can carry the environment although workers serve it: an IP
/// allow list of only private ranges governs it, which no visitor through a
/// tunnel can match (ADR 064 D9). `address` names the tunnel when one would
/// carry it.
fn tunnel_into_private_rule(
    facts: &PlanFacts,
    address: Option<&str>,
    shown: &str,
) -> Option<String> {
    let rule = facts.private_rule.as_ref()?;
    let tunnels_served = !facts.deployed || facts.served_by_workers;
    if !tunnels_served {
        return None;
    }
    let through = address
        .and_then(|host| {
            tunnel_route(facts, host).or_else(|| {
                covering_tunnel(facts, host).map(|tunnel| TunnelRoute {
                    tunnel,
                    route: false,
                    confirmation: None,
                })
            })
        })
        .map(|r| format!("tunnel `{}`", r.tunnel.name))
        .unwrap_or_else(|| "a tunnel".to_string());
    Some(format!(
        "No tunnel is offered: {} The rule is {}.",
        private_allow_list_message(&through, &rule.cidr, shown),
        rule.rule
    ))
}

/// Whether `facts.audience` can work for the environment before the user
/// asks for an address (ADR 069 D3): the plan with no address, reduced to
/// the ways in it offers. Stores nothing. A `tailnet_users` audience is
/// judged before anyone is listed: on whether any way in can tell who a
/// visitor is.
pub fn choice(facts: &PlanFacts) -> AudienceChoice {
    let unasked = PlanFacts {
        requested: None,
        ..facts.clone()
    };
    let planned = if unasked.audience == Audience::TailnetUsers && unasked.principals.is_empty() {
        tailnet_users_ways(&unasked)
    } else {
        plan(&unasked)
    };
    let available = !planned.options.is_empty();
    // A way in left out while another still fits, said where the user
    // chooses rather than after they typed an address.
    let not_offered = if available && !unasked.audience.is_private() {
        let tunnel_offered = planned
            .options
            .iter()
            .any(|o| o.way_in.starts_with("tunnel:"));
        let shown = planned.address.as_deref().unwrap_or("the address");
        let private_rule = tunnel_into_private_rule(&unasked, planned.address.as_deref(), shown)
            .map(|reason| PlanBlocked {
                reason,
                action: None,
            });
        tunnel_unserved(&unasked)
            .into_iter()
            .chain(private_rule)
            .filter(|_| !tunnel_offered)
            .collect()
    } else {
        Vec::new()
    };
    AudienceChoice {
        audience: unasked.audience,
        gate: (unasked.audience == Audience::SignedIn).then_some(unasked.gate),
        available,
        blocked: if available { None } else { planned.blocked },
        suggested_address: planned.address,
        ways_in: planned
            .options
            .into_iter()
            .map(|o| WayInChoice {
                kind: o.kind,
                title: o.title,
                way_in: o.way_in,
                recommended: o.recommended,
            })
            .collect(),
        not_offered,
    }
}

/// The plan for `facts`.
pub fn plan(facts: &PlanFacts) -> ReachabilityPlan {
    let mut blocked_reasons: Vec<PlanBlocked> = Vec::new();
    let gate = (facts.audience == Audience::SignedIn).then_some(facts.gate);
    let empty = |audience, address, address_error, blocked| ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience,
        gate,
        address,
        address_error,
        options: Vec::new(),
        blocked: Some(blocked),
        principals: Vec::new(),
    };

    if let Some(blocked) = facts
        .requested
        .as_deref()
        .map(str::trim)
        .and_then(|host| unchangeable_audience(facts, host))
    {
        return empty(facts.audience, facts.requested.clone(), None, blocked);
    }
    if facts.audience == Audience::Private {
        return plan_private(facts);
    }
    if facts.audience == Audience::TailnetUsers {
        return plan_tailnet_users(facts);
    }
    if !matches!(facts.audience, Audience::Public | Audience::SignedIn) {
        return empty(
            facts.audience,
            facts.requested.clone(),
            None,
            PlanBlocked {
                reason: format!(
                    "Temps cannot plan an address {} here. Choose public, for people who \
                     sign in, or only your private network.",
                    AddressAudience {
                        audience: facts.audience,
                        ..AddressAudience::default()
                    }
                    .describe()
                ),
                action: None,
            },
        );
    }

    // The address: as asked, else a suggestion.
    let (address, address_error) = match planned_address(facts) {
        Ok(address) => (address, None),
        Err(error) => (None, Some(error)),
    };
    if let Some(error) = &address_error {
        return empty(
            facts.audience,
            facts.requested.clone(),
            Some(error.clone()),
            PlanBlocked {
                reason: error.clone(),
                action: None,
            },
        );
    }
    let ask_address = facts
        .requested
        .as_deref()
        .is_none_or(|r| r.trim().is_empty());
    let with_address = |mut fields: Vec<SetupField>| {
        if ask_address {
            fields.insert(0, address_field(address.as_deref()));
        }
        fields
    };
    let shown = address.clone().unwrap_or_else(|| "the address".to_string());
    let attached = address
        .as_ref()
        .is_some_and(|a| facts.attached_here.contains(a));
    let attach_label = if attached {
        format!("{shown} is already on {}", facts.environment_name)
    } else {
        format!("Attach {shown} to {}", facts.environment_name)
    };
    // B5: an address that is private now becomes public, and says so.
    let made_public = address
        .as_deref()
        .filter(|_| facts.audience == Audience::Public)
        .and_then(|host| becomes_public(facts, host));

    // A tunnel only carries what workers serve, and never into an allow
    // list its visitors cannot match (D9).
    let tunnels_served = !facts.deployed || facts.served_by_workers;
    let tunnels_possible = tunnels_served && facts.private_rule.is_none();
    let d9_note = tunnel_into_private_rule(facts, address.as_deref(), &shown);
    if let Some(reason) = &d9_note {
        blocked_reasons.push(PlanBlocked {
            reason: reason.clone(),
            action: None,
        });
    }
    if let Some(unserved) = tunnel_unserved(facts) {
        blocked_reasons.push(unserved);
    }
    let not_deployed_warning = (!facts.deployed).then(|| {
        format!(
            "Nothing is deployed to {} yet: it is published once a deployment runs on a \
             worker.",
            facts.environment_name
        )
    });
    let mut drafts: Vec<OptionDraft> = Vec::new();

    // 0. The address is this environment's host under a tunnel: one switch
    //    gives every environment its host there (ADR 068 D3, D5's fix).
    if let (Some(host), true) = (address.as_deref(), tunnels_possible) {
        if let Some(tunnel) = environment_hosts_tunnel(facts, host) {
            drafts.push(OptionDraft {
                kind: PlanOptionKind::TunnelEnvironmentHosts,
                title: format!("Give every environment an address under {}", tunnel.domain),
                description: format!(
                    "Tunnel {} publishes each environment as <environment>.{}, and keeps \
                     them as environments come, go and are renamed; {host} is this one's. \
                     Cloudflare serves HTTPS.",
                    tunnel.name, tunnel.domain
                ),
                actions: 1,
                way_in: format!("tunnel:{}", tunnel.name),
                steps: vec![
                    step(
                        StepKind::PublishEnvironmentHosts,
                        format!("Publish every environment through tunnel {}", tunnel.name),
                    ),
                    step(
                        StepKind::WaitForPublication,
                        format!("Wait for tunnel {} to publish {host}", tunnel.name),
                    ),
                    step(StepKind::CheckReachability, format!("Check {host}")),
                ],
                fields: Vec::new(),
                warnings: not_deployed_warning.iter().cloned().collect(),
                confirmation: Some(ENVIRONMENT_HOSTS_WARNING.to_string()),
            });
        }
    }

    // 1. A tunnel that would publish the address, or one that covers it
    //    with its hostname choice set (and confirmed).
    let covering = address
        .as_deref()
        .and_then(|host| covering_tunnel(facts, host));
    let route = address
        .as_deref()
        .and_then(|host| tunnel_route(facts, host));
    if let (Some(route), true) = (&route, tunnels_possible) {
        let tunnel = route.tunnel;
        let mut steps = vec![step(StepKind::AttachDomain, attach_label.clone())];
        if route.route {
            steps.push(step(
                StepKind::RouteHostname,
                format!("Route {shown} through tunnel {}", tunnel.name),
            ));
        }
        steps.push(step(
            StepKind::WaitForPublication,
            format!("Wait for tunnel {} to publish it", tunnel.name),
        ));
        steps.push(step(StepKind::CheckReachability, format!("Check {shown}")));
        drafts.push(OptionDraft {
            kind: PlanOptionKind::ExistingTunnel,
            title: format!("Through tunnel {}", tunnel.name),
            description: format!(
                "Tunnel {} serves {}; {shown} is published through it and Cloudflare serves \
                 HTTPS.",
                tunnel.name, tunnel.domain
            ),
            actions: if route.route { 2 } else { 1 },
            way_in: format!("tunnel:{}", tunnel.name),
            steps,
            fields: with_address(Vec::new()),
            warnings: not_deployed_warning.iter().cloned().collect(),
            confirmation: route.confirmation.clone(),
        });
    } else if let (Some(tunnel), true, None) = (covering, tunnels_possible, &route) {
        blocked_reasons.push(PlanBlocked {
            reason: if tunnel.ready {
                format!(
                    "Tunnel {} covers {shown} but runs on no worker. Run it on one first.",
                    tunnel.name
                )
            } else {
                format!(
                    "Tunnel {} covers {shown} but is not ready yet.",
                    tunnel.name
                )
            },
            action: Some(
                ReachAction::new(ActionKind::OpenIngress, "Open the tunnel")
                    .with_href(INGRESS_SETUP_PATH),
            ),
        });
    }

    // Direct ingresses that can take an HTTP-01 certificate.
    let direct = http01_direct(facts);
    if direct.is_none() {
        if let Some(d) = facts.directs.iter().find(|d| !d.ips.is_empty()) {
            blocked_reasons.push(PlanBlocked {
                reason: if let Some(why) = &d.check_failed {
                    format!("{} failed its last reachability check: {why}", d.name)
                } else {
                    format!(
                        "{} does not answer on port 80, which a certificate through HTTP-01 \
                         needs. Add the domain under Domains with DNS-01 instead.",
                        d.name
                    )
                },
                action: Some(
                    ReachAction::new(ActionKind::OpenIngress, "Add a domain")
                        .with_href("/domains/add"),
                ),
            });
        }
    }
    let managed_zone = address
        .as_deref()
        .and_then(|host| zone_for(host, &facts.zones))
        .filter(|z| z.verified);

    // 2. A direct ingress with DNS Temps manages. A record already there is
    //    changed only when Temps manages it, and the plan says so.
    let existing = match (direct, address.as_deref()) {
        (Some(direct), Some(host)) => planned_record(facts, host, record_type_of(direct.ips[0])),
        _ => PlannedRecord::Absent,
    };
    let dns_label = |ip: std::net::IpAddr, zone: &ZoneFact| match &existing {
        PlannedRecord::Present { record } if record.content == ip.to_string() => {
            format!("{shown} already points at {ip} in {}", zone.domain)
        }
        PlannedRecord::Present { record } => format!(
            "Change {shown} from {} to {ip} in {}",
            record.content, zone.domain
        ),
        _ => format!("Point {shown} at {ip} in {}", zone.domain),
    };
    let dns_blocked = match (&existing, managed_zone) {
        (PlannedRecord::Present { record }, Some(zone)) if !record.managed => Some(format!(
            "{shown} already has an {} record ({}) at {} that Temps did not create. Temps never \
             changes a record it does not manage: remove it there, or use another hostname.",
            record.record_type, record.content, zone.provider_name
        )),
        (PlannedRecord::Unknown, Some(zone)) => Some(format!(
            "Temps could not read {shown}'s DNS records at {} just now, so it cannot show what \
             would change. Try again in a minute.",
            zone.provider_name
        )),
        _ => None,
    };
    if let Some(reason) = &dns_blocked {
        blocked_reasons.push(PlanBlocked {
            reason: reason.clone(),
            action: None,
        });
    }
    if let (Some(direct), Some(zone), None) = (direct, managed_zone, &dns_blocked) {
        let ip = direct.ips[0];
        let mut warnings = Vec::new();
        if let PlannedRecord::Present { record } = &existing {
            if record.content != ip.to_string() {
                warnings.push(format!(
                    "{shown} points at {} now; this changes it to {ip}. Undo puts {} back while \
                     nobody else has changed it meanwhile.",
                    record.content, record.content
                ));
            }
        }
        drafts.push(OptionDraft {
            kind: PlanOptionKind::DirectManagedDns,
            title: format!("Straight to {}", direct.name),
            description: format!(
                "Temps points {shown} at {} ({ip}) through {} and issues its certificate.",
                direct.name, zone.provider_name
            ),
            actions: 1,
            way_in: format!("direct:{}", direct.name),
            steps: vec![
                step(StepKind::AttachDomain, attach_label.clone()),
                step(StepKind::SetDnsRecord, dns_label(ip, zone)),
                step(
                    StepKind::IssueCertificate,
                    format!("Issue a certificate for {shown}"),
                ),
                step(StepKind::CheckReachability, format!("Check {shown}")),
            ],
            fields: with_address(Vec::new()),
            warnings,
            confirmation: None,
        });
    }

    // 3 and 4. A new tunnel, on a worker that allows it.
    let tunnel_worker = facts.tunnel_workers.first();
    if tunnels_possible && covering.is_none() {
        match tunnel_worker {
            None => {
                let reason = match facts.workers_refusing.first() {
                    Some(name) => format!(
                        "No worker lets Temps run a tunnel on it: {name} keeps its public \
                         ingress to itself."
                    ),
                    None => "No active worker can run a tunnel.".to_string(),
                };
                blocked_reasons.push(PlanBlocked {
                    reason,
                    action: Some(
                        ReachAction::new(ActionKind::RunCommand, "On a worker, as root, run")
                            .with_command("temps agent ingress delegated"),
                    ),
                });
            }
            Some(worker) => {
                let zone = address.as_deref().map(|host| {
                    zone_for(host, &facts.zones)
                        .filter(|z| z.provider_type == "cloudflare")
                        .map(|z| z.domain.trim_end_matches('.').to_ascii_lowercase())
                        .unwrap_or_else(|| guess_zone(host))
                });
                let tunnel_domain = match (address.as_deref(), zone.as_deref()) {
                    (Some(host), Some(zone)) => Some(tunnel_domain_for(host, zone)),
                    _ => None,
                };
                let mut warnings: Vec<String> = not_deployed_warning.iter().cloned().collect();
                if let (Some(domain), Some(zone)) = (&tunnel_domain, &zone) {
                    if domain != zone {
                        warnings.push(format!(
                            "Cloudflare's free certificate covers *.{zone} only, so HTTPS on \
                             {shown} needs Cloudflare's Advanced Certificate Manager. A \
                             hostname right under {zone} avoids that."
                        ));
                    }
                }
                let confirmation = address
                    .as_deref()
                    .and_then(|host| new_tunnel_confirmation(facts, host));
                let zone_shown = zone.clone().unwrap_or_else(|| "its domain".to_string());
                let domain_shown = tunnel_domain
                    .clone()
                    .unwrap_or_else(|| "its domain".to_string());
                let tail = vec![
                    step(
                        StepKind::ManageDomain,
                        format!("Let Temps manage DNS for {zone_shown}"),
                    ),
                    step(
                        StepKind::CreateTunnel,
                        format!("Create a tunnel for {domain_shown} on {}", worker.name),
                    ),
                    step(
                        StepKind::WaitForTunnel,
                        format!("Wait for {} to connect", worker.name),
                    ),
                    step(StepKind::AttachDomain, attach_label.clone()),
                    step(
                        StepKind::RouteHostname,
                        format!("Publish {shown} through the tunnel"),
                    ),
                    step(StepKind::WaitForPublication, "Wait for it to be published"),
                    step(StepKind::CheckReachability, format!("Check {shown}")),
                ];
                if facts.cloudflare_accounts.is_empty() {
                    let mut steps = vec![
                        step(StepKind::CheckToken, "Check the token's permissions"),
                        step(StepKind::SaveDnsProvider, "Add the Cloudflare account"),
                    ];
                    steps.extend(tail);
                    drafts.push(OptionDraft {
                        kind: PlanOptionKind::NewTunnelWithToken,
                        title: format!("Through a new Cloudflare Tunnel on {}", worker.name),
                        description: format!(
                            "Paste a Cloudflare API token; Temps creates a tunnel on {} and \
                             publishes {shown} through it. Nothing opens a port.",
                            worker.name
                        ),
                        actions: 3,
                        way_in: "tunnel:new".to_string(),
                        steps,
                        fields: with_address(vec![token_field()]),
                        warnings,
                        confirmation,
                    });
                } else {
                    let account_known = address
                        .as_deref()
                        .and_then(|host| zone_for(host, &facts.zones))
                        .is_some_and(|z| z.provider_type == "cloudflare")
                        || facts.cloudflare_accounts.len() == 1;
                    let fields = if account_known {
                        Vec::new()
                    } else {
                        vec![provider_field()]
                    };
                    drafts.push(OptionDraft {
                        kind: PlanOptionKind::NewTunnel,
                        title: format!("Create a tunnel on {}", worker.name),
                        description: format!(
                            "Temps creates a Cloudflare Tunnel on {} for {domain_shown} and \
                             publishes {shown} through it. Nothing opens a port.",
                            worker.name
                        ),
                        actions: 2,
                        way_in: "tunnel:new".to_string(),
                        steps: tail,
                        fields: with_address(fields),
                        warnings,
                        confirmation,
                    });
                }
            }
        }
    }

    // 5. A direct ingress, DNS set by the user.
    if let (Some(direct), None) = (direct, managed_zone) {
        let ip = direct.ips[0];
        drafts.push(OptionDraft {
            kind: PlanOptionKind::DirectManualDns,
            title: format!("Straight to {}, with a DNS record you add", direct.name),
            description: format!(
                "Add an A record for {shown} pointing at {ip}; Temps waits for it, then \
                 issues the certificate."
            ),
            actions: 2,
            way_in: format!("direct:{}", direct.name),
            steps: vec![
                step(StepKind::AttachDomain, attach_label),
                step(
                    StepKind::ShowDnsRecord,
                    format!("Add an A record for {shown} pointing at {ip}"),
                ),
                step(
                    StepKind::IssueCertificate,
                    format!("Issue a certificate for {shown}"),
                ),
                step(StepKind::CheckReachability, format!("Check {shown}")),
            ],
            fields: with_address(Vec::new()),
            warnings: Vec::new(),
            confirmation: None,
        });
    }

    if let Some(warning) = &made_public {
        for draft in &mut drafts {
            let at = draft
                .steps
                .iter()
                .position(|s| s.kind == StepKind::AttachDomain)
                .map_or(0, |i| i + 1);
            draft.steps.insert(
                at,
                step(StepKind::SetAudience, format!("Make {shown} public")),
            );
            draft.warnings.insert(0, warning.clone());
        }
    }
    if let Some(host) = address.as_deref() {
        for draft in &mut drafts {
            add_edge_rule_steps(facts, host, draft);
        }
    }
    if facts.audience == Audience::SignedIn {
        if facts.gate == GateKind::CloudflareAccess {
            if let Err(blocked) = access_drafts(facts, &shown, &mut drafts) {
                return empty(facts.audience, address, None, blocked);
            }
        }
        signed_in_drafts(facts, address.as_deref(), &shown, &mut drafts);
    }
    let options: Vec<PlanOption> = drafts
        .into_iter()
        .enumerate()
        .map(|(index, draft)| PlanOption {
            kind: draft.kind,
            title: draft.title,
            description: draft.description,
            actions: draft.actions,
            recommended: index == 0,
            way_in: draft.way_in,
            steps: draft.steps,
            fields: draft.fields,
            warnings: draft.warnings,
            confirmation: draft.confirmation,
            required_permissions: permissions_for(draft.kind)
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
        .collect();
    let mut options = options;
    if let Some(note) = &d9_note {
        for option in &mut options {
            option.warnings.push(note.clone());
        }
    }
    let blocked = if options.is_empty() {
        Some(
            blocked_reasons.into_iter().next().unwrap_or(PlanBlocked {
                reason:
                    "Nothing in this cluster can take public traffic yet: no tunnel, no worker \
                     that allows one, and no direct ingress."
                        .to_string(),
                action: Some(
                    ReachAction::new(ActionKind::OpenIngress, "Set up ingress")
                        .with_href(INGRESS_SETUP_PATH),
                ),
            }),
        )
    } else {
        None
    };
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: facts.audience,
        gate,
        address,
        address_error: None,
        options,
        blocked,
        principals: Vec::new(),
    }
}

/// Said on every option that adds the Cloudflare edge rules (ADR 070 D3).
pub fn edge_rules_warning(host: &str) -> String {
    format!(
        "Temps adds two Cloudflare rules for {host} only: a redirect from http:// to https:// \
         (the token needs {}) and a WAF rule refusing Cloudflare's other ports such as 8080 \
         and 8443 (the token needs {}; it can change every WAF custom rule in the zone). \
         Without a permission Temps still publishes {host} and says which one is missing.",
        EdgeRulePurpose::HttpsRedirect.permission(),
        EdgeRulePurpose::StandardPorts.permission(),
    )
}

/// What a tunnel address may answer besides HTTPS, as offered for `host`
/// (ADR 070 D1): the choices, and the one taken when the user does not
/// answer, which is the address's recorded choice, else HTTPS only. A
/// hostname below the free certificate's reach cannot be HTTPS only:
/// redirecting it to an HTTPS address without a certificate would take it
/// down.
pub fn http_policy_choices(facts: &PlanFacts, host: &str) -> (Vec<HttpPolicy>, HttpPolicy) {
    // Only a zone Temps knows: a managed one, else the covering tunnel's.
    // A guess (the last two labels) is wrong for `example.co.uk` and would
    // refuse HTTPS only to every address in it; with no known zone the run
    // checks the zone Cloudflare names before adding the redirect.
    let zone = zone_for(host, &facts.zones)
        .map(|z| z.domain.clone())
        .or_else(|| covering_tunnel(facts, host).and_then(|t| t.zone.clone()))
        .map(|z| z.trim_end_matches('.').to_ascii_lowercase());
    let allowed: Vec<HttpPolicy> = if zone
        .as_deref()
        .is_some_and(|zone| below_free_certificate(host, zone))
    {
        vec![HttpPolicy::AllowHttp, HttpPolicy::AllowAll]
    } else {
        HttpPolicy::ALL.to_vec()
    };
    let default = facts
        .http_policies
        .get(host)
        .copied()
        .filter(|p| allowed.contains(p))
        .unwrap_or(allowed[0]);
    (allowed, default)
}

/// Whether an option publishes its address through a Cloudflare tunnel,
/// and so offers the HTTPS-only choice (ADR 070).
pub fn offers_http_policy(kind: PlanOptionKind) -> bool {
    matches!(
        kind,
        PlanOptionKind::ExistingTunnel
            | PlanOptionKind::NewTunnel
            | PlanOptionKind::NewTunnelWithToken
    )
}

fn http_policy_field(host: &str, allowed: &[HttpPolicy], default: HttpPolicy) -> SetupField {
    let label = |policy: HttpPolicy| match policy {
        HttpPolicy::HttpsOnly => "HTTPS only (recommended)".to_string(),
        HttpPolicy::AllowHttp => "Also plain HTTP on port 80".to_string(),
        HttpPolicy::AllowAll => "Also plain HTTP, and Cloudflare's other ports".to_string(),
    };
    let help = |policy: HttpPolicy| match policy {
        HttpPolicy::HttpsOnly => format!(
            "http://{host} redirects to HTTPS and Cloudflare's other ports (8080, 8443, …) \
             are refused, with two Cloudflare rules for this hostname only."
        ),
        HttpPolicy::AllowHttp => format!(
            "http://{host} serves the app unencrypted; only Cloudflare's other ports are \
             refused."
        ),
        HttpPolicy::AllowAll => format!(
            "No Cloudflare rule: {host} answers plain HTTP and on every port Cloudflare \
             proxies."
        ),
    };
    SetupField {
        key: FIELD_HTTP_POLICY.to_string(),
        label: "What it answers".to_string(),
        help: Some(format!(
            "Left unanswered: {}. Temps keeps the choice on the address, so making it \
             reachable again offers the same. Choosing less than before leaves the rules an \
             earlier run added; undo that run to remove them.",
            label(default)
        )),
        kind: FieldKind::Choice {
            choices: allowed
                .iter()
                .map(|p| FieldChoice {
                    value: p.as_str().to_string(),
                    label: label(*p),
                    help: Some(help(*p)),
                })
                .collect(),
        },
        required: false,
        placeholder: Some(default.as_str().to_string()),
    }
}

/// The steps an option publishing `host` through a tunnel takes for
/// `policy` (ADR 070 D1): the rules it wants, before the address is
/// attached, and recording the choice right after.
pub fn http_policy_steps(host: &str, policy: HttpPolicy) -> (Vec<PlanStep>, PlanStep) {
    let mut rules = Vec::new();
    if policy.wants(EdgeRulePurpose::HttpsRedirect) {
        rules.push(step(
            StepKind::RedirectToHttps,
            format!(
                "Redirect http://{host} to HTTPS, with a Cloudflare redirect rule for this \
                 hostname only"
            ),
        ));
    }
    if policy.wants(EdgeRulePurpose::StandardPorts) {
        rules.push(step(
            StepKind::StandardPortsOnly,
            format!(
                "Refuse {host} on Cloudflare's other ports (8080, 8443, …), with a Cloudflare \
                 WAF rule for this hostname only"
            ),
        ));
    }
    let record = step(
        StepKind::SetHttpPolicy,
        format!("Keep {host} at {}", policy.describe()),
    );
    (rules, record)
}

/// Put `policy`'s steps into `steps`: rules before attaching, the record
/// right after. Any earlier policy steps are replaced, in the places `plan`
/// gives them: the rules ahead of an Access app made just before attaching,
/// the record after an audience set just after it, so applying a choice
/// keeps the order the plan showed.
pub fn place_http_policy_steps(steps: &mut Vec<PlanStep>, host: &str, policy: HttpPolicy) {
    steps.retain(|s| {
        !matches!(
            s.kind,
            StepKind::RedirectToHttps | StepKind::StandardPortsOnly | StepKind::SetHttpPolicy
        )
    });
    let (rules, record) = http_policy_steps(host, policy);
    let attach = steps.iter().position(|s| s.kind == StepKind::AttachDomain);
    let mut at = attach.unwrap_or(0);
    if at > 0 && steps[at - 1].kind == StepKind::AccessApp {
        at -= 1;
    }
    let count = rules.len();
    for (offset, added) in rules.into_iter().enumerate() {
        steps.insert(at + offset, added);
    }
    let after = match attach.map(|i| i + count) {
        Some(attach) if steps.get(attach + 1).map(|s| s.kind) == Some(StepKind::SetAudience) => {
            attach + 2
        }
        Some(attach) => attach + 1,
        None => at + count,
    };
    steps.insert(after.min(steps.len()), record);
}

/// The HTTPS-only choice an applied `option` takes (ADR 070 D1): the
/// answer, else the default the plan showed. `None` for an option that
/// offers no choice; `Err` for an answer the option does not offer.
pub fn chosen_http_policy(
    option: &PlanOption,
    answer: Option<&str>,
) -> Result<Option<HttpPolicy>, String> {
    let Some(field) = option.fields.iter().find(|f| f.key == FIELD_HTTP_POLICY) else {
        return Ok(None);
    };
    let FieldKind::Choice { choices } = &field.kind else {
        return Ok(None);
    };
    let offered: Vec<HttpPolicy> = choices
        .iter()
        .filter_map(|c| HttpPolicy::parse(&c.value))
        .collect();
    let value = answer.or(field.placeholder.as_deref()).unwrap_or_default();
    match HttpPolicy::parse(value).filter(|p| offered.contains(p)) {
        Some(policy) => Ok(Some(policy)),
        None => Err(format!(
            "`{value}` is not one of the choices for what the address answers: {}.",
            offered
                .iter()
                .map(|p| format!("`{}`", p.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// ADR 070 D1: an option that publishes `host` through a Cloudflare tunnel
/// asks what the address answers besides HTTPS, HTTPS only by default, and
/// shows the steps of the default.
fn add_edge_rule_steps(facts: &PlanFacts, host: &str, draft: &mut OptionDraft) {
    if !offers_http_policy(draft.kind) {
        return;
    }
    let (allowed, default) = http_policy_choices(facts, host);
    place_http_policy_steps(&mut draft.steps, host, default);
    draft
        .fields
        .push(http_policy_field(host, &allowed, default));
    if !allowed.contains(&HttpPolicy::HttpsOnly) {
        draft.warnings.push(format!(
            "HTTPS only is not offered for {host}: Cloudflare's free certificate covers one \
             level below the zone only, and a redirect to an HTTPS address without a \
             certificate would take {host} down. Once HTTPS works on it, add a redirect rule \
             in Cloudflare (Rules → Redirect Rules)."
        ));
    }
    if let Some(exposure) = default.exposure(host) {
        draft.warnings.push(exposure);
    }
    if default.wants(EdgeRulePurpose::StandardPorts) {
        draft.warnings.push(edge_rules_warning(host));
    }
}

/// What a Cloudflare Access plan changes in the drafts (ADR 065 D9): only
/// ways in through a tunnel stay, since Access sees nothing else, and the
/// app in front of the address is used as found or created before the
/// address is attached. `Err` when no option can be offered.
fn access_drafts(
    facts: &PlanFacts,
    shown: &str,
    drafts: &mut Vec<OptionDraft>,
) -> Result<(), PlanBlocked> {
    let before = drafts.len();
    drafts.retain(|d| d.way_in.starts_with("tunnel"));
    let dropped = before - drafts.len();
    if drafts.is_empty() {
        return Err(PlanBlocked {
            reason: format!(
                "Cloudflare Access only sees traffic that comes through a Cloudflare tunnel, \
                 and no tunnel can carry {shown} here. Add a worker that allows a tunnel, or \
                 use the Temps sign-in instead."
            ),
            action: Some(
                ReachAction::new(ActionKind::OpenIngress, "Set up ingress")
                    .with_href(INGRESS_SETUP_PATH),
            ),
        });
    }
    let (step_label, fields, warning) = match access_choice(facts) {
        AccessChoice::NoProvider => {
            return Err(PlanBlocked {
                reason: "No Cloudflare Access token is saved. Add one with Access: Apps and \
                         Policies Edit and Access: Organizations, Identity Providers, and Groups \
                         Read, then plan again."
                    .to_string(),
                action: Some(
                    ReachAction::new(ActionKind::OpenIngress, "Add a Cloudflare Access token")
                        .with_href(ACCESS_SETUP_PATH),
                ),
            })
        }
        AccessChoice::Unreadable { provider, error } => {
            return Err(PlanBlocked {
                reason: format!(
                    "Temps could not read the Access applications of {}, so it cannot tell \
                     whether one already protects {shown}: {error} Try again.",
                    provider.name
                ),
                action: Some(
                    ReachAction::new(ActionKind::OpenIngress, "Check the Access token")
                        .with_href(ACCESS_SETUP_PATH),
                ),
            })
        }
        AccessChoice::Existing { provider, app } => (
            format!(
                "Use Cloudflare Access app {} ({}) in front of {shown}",
                app.name, provider.name
            ),
            Vec::new(),
            format!(
                "Access app {} already protects {shown}: Temps uses it as it is and never \
                 edits or deletes it. Its policies decide who gets in.",
                app.name
            ),
        ),
        AccessChoice::Create(providers) => {
            let mut fields = Vec::new();
            if providers.len() > 1 {
                fields.push(SetupField {
                    key: FIELD_ACCESS_PROVIDER.to_string(),
                    label: "Cloudflare Access account".to_string(),
                    help: Some("The account the Access app is created in.".to_string()),
                    kind: FieldKind::Choice {
                        choices: providers
                            .iter()
                            .map(|p| FieldChoice {
                                value: p.id.to_string(),
                                label: p.name.clone(),
                                help: None,
                            })
                            .collect(),
                    },
                    required: true,
                    placeholder: None,
                });
            }
            fields.extend(access_policy_fields());
            (
                format!("Create a Cloudflare Access app for {shown}"),
                fields,
                format!(
                    "Cloudflare Access decides who can open {shown}: only the email addresses \
                     and domains you give get in. Undo deletes the app again unless someone \
                     edited it in Cloudflare."
                ),
            )
        }
    };
    for draft in drafts.iter_mut() {
        let at = draft
            .steps
            .iter()
            .position(|s| s.kind == StepKind::AttachDomain)
            .unwrap_or(0);
        draft
            .steps
            .insert(at, step(StepKind::AccessApp, step_label.clone()));
        draft.fields.extend(fields.iter().cloned());
        draft.warnings.insert(0, warning.clone());
        if dropped > 0 {
            draft.warnings.push(
                "Ways in that reach Temps directly are not offered: Cloudflare Access only \
                 sees traffic that comes through a Cloudflare tunnel."
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Who a new Access app lets in.
fn access_policy_fields() -> Vec<SetupField> {
    vec![
        SetupField {
            key: FIELD_ACCESS_EMAILS.to_string(),
            label: "Email addresses".to_string(),
            help: Some("People who may open it, one email address each.".to_string()),
            kind: FieldKind::TextList,
            required: false,
            placeholder: Some("ana@example.com".to_string()),
        },
        SetupField {
            key: FIELD_ACCESS_EMAIL_DOMAINS.to_string(),
            label: "Email domains".to_string(),
            help: Some(
                "Everyone with an email address at these domains. Give at least one address \
                 or domain."
                    .to_string(),
            ),
            kind: FieldKind::TextList,
            required: false,
            placeholder: Some("example.com".to_string()),
        },
    ]
}

/// Who a new Access app lets in, from an apply's answers: lists of
/// strings (or one comma-separated string).
pub fn access_policy_answer(
    answers: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<AccessPolicyAnswer, String> {
    let list = |key: &str| -> Result<Vec<String>, String> {
        match answers.get(key) {
            None | Some(serde_json::Value::Null) => Ok(Vec::new()),
            Some(serde_json::Value::String(text)) => {
                Ok(text.split([',', '\n']).map(str::to_string).collect())
            }
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{key} must be a list of text values."))
                })
                .collect(),
            Some(_) => Err(format!("{key} must be a list of text values.")),
        }
    };
    let answer = AccessPolicyAnswer {
        emails: list(FIELD_ACCESS_EMAILS)?,
        email_domains: list(FIELD_ACCESS_EMAIL_DOMAINS)?,
    }
    .normalized();
    match answer.problem() {
        Some(problem) => Err(problem),
        None => Ok(answer),
    }
}

/// An address whose current audience this version cannot serve is left as
/// it is: a run would otherwise widen it, and its undo could not write the
/// value back (ADR 065 D6).
fn unchangeable_audience(facts: &PlanFacts, host: &str) -> Option<PlanBlocked> {
    let (_, current) = facts
        .audiences
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(host))?;
    (!current.is_served()).then(|| PlanBlocked {
        reason: format!(
            "{host} is set to an audience this version of Temps cannot serve ({}), so it is \
             left as it is. Change it with the version that set it, or choose another \
             hostname.",
            AddressAudience {
                audience: *current,
                ..AddressAudience::default()
            }
            .describe()
        ),
        action: None,
    })
}

/// What a signed-in plan adds to every way in (ADR 065 D7): the gate is
/// set right after the address is attached, before anything publishes it,
/// so no listener serves it open; who may sign in is asked; and a password
/// the environment already has is said to apply as well.
fn signed_in_drafts(
    facts: &PlanFacts,
    address: Option<&str>,
    shown: &str,
    drafts: &mut [OptionDraft],
) {
    let current = address.and_then(|host| {
        facts
            .audiences
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(host))
            .map(|(_, audience)| *audience)
    });
    let access = facts.gate == GateKind::CloudflareAccess;
    let mut warnings = vec![if access {
        format!(
            "Only people Cloudflare Access lets in can open {shown}, and only through the \
             tunnel. Changing who can open it needs a recent password or passkey \
             confirmation."
        )
    } else {
        format!(
            "Only people who sign in to Temps, with access to this project (and on the list, \
             if you give one), can open {shown}. Everyone else gets the Temps sign-in. \
             Changing who can open it needs a recent password or passkey confirmation."
        )
    }];
    if current == Some(Audience::Private) {
        warnings.push(format!(
            "{shown} is private now. This puts it on a public way in behind the sign-in: \
             anyone allowed to sign in can open it from anywhere, not only from your private \
             network."
        ));
    }
    if let Some(warning) = still_open_warning(facts, address, shown) {
        warnings.push(warning);
    }
    if facts.password_protected {
        warnings.push(format!(
            "{} also has password protection. Both apply: visitors sign in first, then enter \
             the password.",
            facts.environment_name
        ));
    }
    let label = if access {
        format!("Let only people Cloudflare Access admits open {shown}")
    } else {
        format!("Put the Temps sign-in in front of {shown}")
    };
    for draft in drafts.iter_mut() {
        let at = draft
            .steps
            .iter()
            .position(|s| s.kind == StepKind::AttachDomain)
            .map_or(0, |i| i + 1);
        draft
            .steps
            .insert(at, step(StepKind::SetAudience, label.clone()));
        if !access {
            draft.fields.extend(gate_list_fields());
        }
        for (index, warning) in warnings.iter().enumerate() {
            draft.warnings.insert(index, warning.clone());
        }
    }
}

/// The sign-in is per address: every other public way to the same app
/// stays open to anyone, and the plan names each one rather than imply the
/// app is behind the sign-in. `None` when there is no other.
fn still_open_warning(facts: &PlanFacts, address: Option<&str>, shown: &str) -> Option<String> {
    let mut open: Vec<&str> = facts
        .audiences
        .iter()
        .filter(|(host, audience)| {
            *audience == Audience::Public && address.is_none_or(|a| !a.eq_ignore_ascii_case(host))
        })
        .map(|(host, _)| host.as_str())
        .chain(facts.service_addresses.iter().map(String::as_str))
        .collect();
    open.sort_unstable();
    open.dedup();
    if open.is_empty() {
        return None;
    }
    let list = open.join(", ");
    Some(format!(
        "The sign-in covers {shown} only. The same app stays open to anyone at {list}. Make \
         those signed-in or detach them if they shouldn't be public; the generated address \
         and Compose service addresses can't be signed-in yet."
    ))
}

/// Who may sign in, besides having access to the project: both optional.
fn gate_list_fields() -> Vec<SetupField> {
    vec![
        SetupField {
            key: FIELD_GATE_TEAMS.to_string(),
            label: "Only these teams".to_string(),
            help: Some(
                "Leave empty to let everyone with access to the project sign in.".to_string(),
            ),
            kind: FieldKind::Teams,
            required: false,
            placeholder: None,
        },
        SetupField {
            key: FIELD_GATE_USERS.to_string(),
            label: "Only these people".to_string(),
            help: Some(
                "People who may sign in even when they are in none of the teams above.".to_string(),
            ),
            kind: FieldKind::Users,
            required: false,
            placeholder: None,
        },
    ]
}

/// The list a signed-in apply was answered with: team and user ids, as
/// numbers or numeric strings. `None` for no list.
pub fn gate_allow_answer(
    answers: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<Option<temps_core::gate::GateAllowList>, String> {
    let ids = |key: &str, what: &str| -> Result<Vec<i32>, String> {
        match answers.get(key) {
            None | Some(serde_json::Value::Null) => Ok(Vec::new()),
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .map(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                        .and_then(|id| i32::try_from(id).ok())
                        .filter(|id| *id > 0)
                        .ok_or_else(|| format!("{what} must be a list of {what} ids ({key})."))
                })
                .collect(),
            Some(_) => Err(format!("{what} must be a list of {what} ids ({key}).")),
        }
    };
    let list = temps_core::gate::GateAllowList {
        teams: ids(FIELD_GATE_TEAMS, "teams")?,
        users: ids(FIELD_GATE_USERS, "users")?,
    }
    .normalized();
    Ok((!list.is_empty()).then_some(list))
}

/// When `host` is attached here as a private address: what making it
/// public exposes, in words (ADR 064 Security).
pub fn becomes_public(facts: &PlanFacts, host: &str) -> Option<String> {
    let (_, audience) = facts.audiences.iter().find(|(a, _)| a == host)?;
    if *audience == Audience::Public {
        return None;
    }
    if *audience == Audience::SignedIn {
        return Some(format!(
            "{host} is for people who sign in now. This makes it public: the sign-in in \
             front of it goes, and anyone on the internet can open {host} and everything {} \
             serves there. It needs a recent password or passkey confirmation.",
            facts.environment_name
        ));
    }
    let range = facts
        .private_ranges
        .iter()
        .find(|(a, _)| a == host)
        .and_then(|(_, r)| r.clone());
    Some(format!(
        "{host} is {} now. This makes it public: anyone on the internet can open {host} and \
         everything {} serves there, not only devices on your private network. It needs a \
         recent password or passkey confirmation.",
        match range {
            Some(range) => format!("private to {range}"),
            None => "private".to_string(),
        },
        facts.environment_name
    ))
}

/// A private plan that offers nothing, and why.
fn private_blocked(
    address: Option<String>,
    address_error: Option<String>,
    blocked: PlanBlocked,
) -> ReachabilityPlan {
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        gate: None,
        address,
        address_error,
        options: Vec::new(),
        blocked: Some(blocked),
        principals: Vec::new(),
    }
}

/// What a private address is as a tailnet name (ADR 065 D1).
enum TailnetName<'a> {
    /// A worker's own name.
    Machine(&'a TailnetWorkerFact),
    /// Under a tailnet, and no machine's name: only a Service gives it.
    UnderTailnet,
    Other,
}

fn tailnet_name<'a>(facts: &'a PlanFacts, host: &str) -> TailnetName<'a> {
    let host = temps_core::tailnet::normalize_name(host);
    if let Some(worker) = facts
        .tailnet_workers
        .iter()
        .find(|w| w.report.dns_name.as_deref() == Some(host.as_str()))
    {
        return TailnetName::Machine(worker);
    }
    let under = |suffix: &str| host.ends_with(&format!(".{suffix}"));
    if under("ts.net")
        || facts
            .tailnet_workers
            .iter()
            .filter_map(|w| w.report.tailnet.as_deref())
            .any(under)
    {
        TailnetName::UnderTailnet
    } else {
        TailnetName::Other
    }
}

/// Workers whose owner allowed Tailscale Serve, the one the environment
/// runs on first.
fn serve_candidates(facts: &PlanFacts) -> Vec<&TailnetWorkerFact> {
    let mut workers: Vec<&TailnetWorkerFact> = facts
        .tailnet_workers
        .iter()
        .filter(|w| w.report.consent)
        .collect();
    workers.sort_by_key(|w| Some(&w.name) != facts.suggested_private_worker.as_ref());
    workers
}

/// Why `worker` cannot serve its machine name for this app now, with what
/// fixes it (ADR 065 D5's messages), in the order the agent decides them.
pub fn machine_name_problem(
    facts: &PlanFacts,
    worker: &TailnetWorkerFact,
) -> Option<(String, Option<ReachAction>)> {
    let node = worker.name.as_str();
    let report = &worker.report;
    if let Some(problem) = tailnet_worker_problem(node, report, worker.minutes_ago) {
        return Some(problem);
    }
    let name = report.dns_name.clone().unwrap_or_default();
    let problem = |message: String| Some((message, None));
    if let Some(listener) = worker.listener_on_tailscale {
        return problem(format!(
            "{}.",
            tailnet_messages::listener_exclusive(node, &listener.to_string())
        ));
    }
    if let Some(held_by) = &report.port_443_held_by {
        return problem(tailnet_messages::slot_taken(&name, held_by));
    }
    if report.port_443_funnel {
        return problem(tailnet_messages::funnel(node));
    }
    if facts
        .serve_entries
        .iter()
        .any(|e| e.node_id == worker.id && e.hostname == name && e.removing)
    {
        return problem(format!(
            "An earlier setup of `{name}` is still being taken off {node}'s Serve config. Try \
             again in a minute."
        ));
    }
    None
}

/// Why `worker` cannot host a Tailscale Service now (ADR 065 D5): what
/// keeps tailscaled from serving at all, then a device with no tag.
pub fn service_host_problem(worker: &TailnetWorkerFact) -> Option<(String, Option<ReachAction>)> {
    let node = worker.name.as_str();
    let report = &worker.report;
    if let Some(problem) = tailnet_worker_problem(node, report, worker.minutes_ago) {
        return Some(problem);
    }
    if report.tags.is_empty() {
        let machine = report.dns_name.clone().unwrap_or_default();
        return Some((
            tailnet_messages::not_tagged(
                node,
                report.login.as_deref().unwrap_or("a user"),
                &machine,
            ),
            Some(
                ReachAction::new(ActionKind::RunCommand, format!("On {node}, as root, run"))
                    .with_command(format!(
                        "sudo tailscale up --advertise-tags={}",
                        tailscale::DEFAULT_TAG
                    )),
            ),
        ));
    }
    None
}

/// The tag a Service on `worker` is for: `tag:temps` when the worker has
/// it, else its first (ADR 065 D4).
pub fn service_tag(worker: &TailnetWorkerFact) -> String {
    let tags = &worker.report.tags;
    tags.iter()
        .find(|t| t.as_str() == tailscale::DEFAULT_TAG)
        .or_else(|| tags.first())
        .cloned()
        .unwrap_or_else(|| tailscale::DEFAULT_TAG.to_string())
}

/// The Service `host` names, with its tailnet: one valid label directly
/// under the MagicDNS suffix of a worker that reports one, and not a
/// machine's name. `myapp.example.ts.net` → `(svc:myapp, example.ts.net)`.
pub fn service_of(facts: &PlanFacts, host: &str) -> Option<(String, String)> {
    let host = temps_core::tailnet::normalize_name(host);
    if facts
        .tailnet_workers
        .iter()
        .any(|w| w.report.dns_name.as_deref() == Some(host.as_str()))
    {
        return None;
    }
    facts
        .tailnet_workers
        .iter()
        .filter_map(|w| w.report.tailnet.as_deref())
        .find_map(|tailnet| {
            let label = host.strip_suffix(&format!(".{tailnet}"))?;
            temps_core::tailnet::valid_service_label(label)
                .then(|| (tailscale::service_name(label), tailnet.to_string()))
        })
}

/// The worker that hosts a Service on `tailnet`: one whose owner allowed
/// Serve and that can host one now, the environment's first.
pub fn service_worker<'a>(facts: &'a PlanFacts, tailnet: &str) -> Option<&'a TailnetWorkerFact> {
    serve_candidates(facts)
        .into_iter()
        .filter(|w| w.report.tailnet.as_deref() == Some(tailnet))
        .find(|w| service_host_problem(w).is_none())
}

/// The cluster's credential, when a run can use it.
fn usable_credential(facts: &PlanFacts) -> Option<&TailscaleCredentialFact> {
    facts
        .tailscale_credential
        .as_ref()
        .filter(|c| c.refusal.is_none())
}

/// A Service address for this app on the first tailnet with a worker that
/// can host one: `<project>-<environment>.<tailnet>` (ADR 065 D1).
fn suggested_service_address(facts: &PlanFacts) -> Option<String> {
    let label = suggested_label(facts);
    if !temps_core::tailnet::valid_service_label(&label) {
        return None;
    }
    serve_candidates(facts)
        .into_iter()
        .filter(|w| service_host_problem(w).is_none())
        .filter_map(|w| w.report.tailnet.as_deref())
        .map(|tailnet| format!("{label}.{tailnet}"))
        .find(|c| !facts.taken_elsewhere.contains(c))
}

/// The address a private plan is for: as asked, else in ADR 065 D1's
/// order a Service when the cluster has a usable Tailscale credential, the
/// name of a worker that can serve it through Tailscale Serve, one under a
/// zone Temps manages (which a listener needs for its record and its DNS-01
/// certificate), and last a Service after pasting a credential.
pub fn planned_private_address(facts: &PlanFacts) -> Result<Option<String>, String> {
    if facts
        .requested
        .as_deref()
        .is_some_and(|r| !r.trim().is_empty())
    {
        return planned_address(facts);
    }
    let free = |c: &String| !facts.taken_elsewhere.contains(c);
    if usable_credential(facts).is_some() {
        if let Some(address) = suggested_service_address(facts) {
            return Ok(Some(address));
        }
    }
    if let Some(name) = serve_candidates(facts)
        .into_iter()
        .filter(|w| machine_name_problem(facts, w).is_none())
        .filter_map(|w| w.report.dns_name.clone())
        .find(free)
    {
        return Ok(Some(name));
    }
    let label = suggested_label(facts);
    if let Some(address) = facts
        .zones
        .iter()
        .filter(|z| z.verified)
        .map(|z| format!("{label}.{}", z.domain.trim_end_matches('.')))
        .find(|c| free(c) && !facts.attached_here.contains(c))
    {
        return Ok(Some(address));
    }
    Ok(suggested_service_address(facts))
}

/// The plan for a private address: a worker's own tailnet name through
/// Tailscale Serve (ADR 065 D1), or its private listener (ADR 064 D5).
fn plan_private(facts: &PlanFacts) -> ReachabilityPlan {
    let ask_address = facts
        .requested
        .as_deref()
        .is_none_or(|r| r.trim().is_empty());
    let address = match planned_private_address(facts) {
        Ok(address) => address,
        Err(error) => {
            return private_blocked(
                facts.requested.clone(),
                Some(error.clone()),
                PlanBlocked {
                    reason: error,
                    action: None,
                },
            )
        }
    };
    match address.as_deref().map(|a| tailnet_name(facts, a)) {
        Some(TailnetName::Machine(worker)) => plan_machine_name(facts, worker, address.clone()),
        Some(TailnetName::UnderTailnet)
            if address
                .as_deref()
                .is_some_and(|a| service_of(facts, a).is_some()) =>
        {
            plan_service(facts, address.clone())
        }
        Some(TailnetName::UnderTailnet) => {
            let reason = tailnet_messages::service_only(address.as_deref().unwrap_or_default());
            private_blocked(
                address,
                None,
                PlanBlocked {
                    reason,
                    action: None,
                },
            )
        }
        Some(TailnetName::Other) | None => {
            let listener = plan_listener(facts, address, ask_address);
            // Nothing asked for and no listener fits: a worker that allowed
            // Serve says what keeps its name from working instead.
            if listener.options.is_empty() && ask_address {
                if let Some((reason, action)) = serve_candidates(facts)
                    .first()
                    .and_then(|w| machine_name_problem(facts, w))
                {
                    return private_blocked(listener.address, None, PlanBlocked { reason, action });
                }
            }
            listener
        }
    }
}

/// The plan for `tailnet_users` (ADR 065 D6): a private plan, kept to the
/// ways in that can tell who a visitor is, on agents that check it.
fn plan_tailnet_users(facts: &PlanFacts) -> ReachabilityPlan {
    if facts.principals.is_empty() {
        return tailnet_users_blocked(
            facts,
            facts.requested.clone(),
            tailnet_messages::NO_PRINCIPALS.to_string(),
            None,
        );
    }
    tailnet_users_ways(facts)
}

fn tailnet_users_blocked(
    facts: &PlanFacts,
    address: Option<String>,
    reason: String,
    action: Option<ReachAction>,
) -> ReachabilityPlan {
    let mut plan = private_blocked(address, None, PlanBlocked { reason, action });
    plan.audience = Audience::TailnetUsers;
    plan.principals = facts.principals.clone();
    plan
}

/// The ways in a `tailnet_users` plan keeps, whoever is listed: those that
/// can tell who a visitor is, on agents that check it. [`choice`] calls it
/// before anyone is listed; a plan to apply goes through
/// [`plan_tailnet_users`], which refuses an empty list.
fn tailnet_users_ways(facts: &PlanFacts) -> ReachabilityPlan {
    let blocked = |address: Option<String>, reason: String, action: Option<ReachAction>| {
        tailnet_users_blocked(facts, address, reason, action)
    };
    let mut planned = plan_private(facts);
    planned.audience = Audience::TailnetUsers;
    planned.principals = facts.principals.clone();
    let who = AddressAudience {
        audience: Audience::TailnetUsers,
        principals: facts.principals.clone(),
        ..AddressAudience::default()
    }
    .describe();
    let listed = facts
        .principals
        .iter()
        .map(|p| p.value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let shown = planned
        .address
        .clone()
        .unwrap_or_else(|| "the address".to_string());
    let mut refusals: Vec<(String, Option<ReachAction>)> = Vec::new();
    planned.options.retain_mut(|option| {
        let node = option
            .way_in
            .strip_prefix("private:")
            .unwrap_or_default()
            .to_string();
        if option.kind == PlanOptionKind::PrivateWorker {
            let Some((worker, ip)) = private_worker(facts) else {
                return false;
            };
            // D6: whois answers only for Tailscale peers, and asking it
            // needs the worker owner's consent.
            if !worker.tailscale {
                refusals.push((tailnet_messages::users_need_tailnet(&ip.to_string()), None));
                return false;
            }
            let consent = facts
                .tailnet_workers
                .iter()
                .any(|w| w.id == worker.id && w.report.consent);
            if !consent {
                refusals.push((
                    tailnet_messages::users_need_consent(&worker.name),
                    Some(
                        ReachAction::new(
                            ActionKind::RunCommand,
                            format!("On {}, as root, run", worker.name),
                        )
                        .with_command(temps_core::tailnet::TAILSCALE_SERVE_COMMAND),
                    ),
                ));
                return false;
            }
        }
        if !facts.identity_workers.contains(&node) {
            refusals.push((
                format!(
                    "{shown} admits only listed tailnet users, and the agent on `{node}` cannot \
                     check who they are. Update the agent on `{node}`, then plan again."
                ),
                None,
            ));
            return false;
        }
        for step in &mut option.steps {
            if step.kind == StepKind::SetAudience {
                step.label = format!("Make {shown} private to your tailnet, {who}");
            }
        }
        option.warnings.insert(
            0,
            format!(
                "Only {listed} can open {shown}. Anyone else on your tailnet gets a page that \
                 says it admits only listed tailnet users. Changing who is listed needs a \
                 recent password or passkey confirmation."
            ),
        );
        true
    });
    for (index, option) in planned.options.iter_mut().enumerate() {
        option.recommended = index == 0;
    }
    if planned.options.is_empty() && planned.blocked.is_none() {
        let (reason, action) = refusals.into_iter().next().unwrap_or_else(|| {
            (
                "No way in that can tell who a tailnet visitor is is available.".to_string(),
                None,
            )
        });
        return blocked(planned.address.clone(), reason, action);
    }
    planned
}

/// The plan for a worker's own tailnet name (ADR 065 D1 option 3):
/// tailscaled serves it and gets its certificate, so nothing is written to
/// DNS or `domains`.
fn plan_machine_name(
    facts: &PlanFacts,
    worker: &TailnetWorkerFact,
    address: Option<String>,
) -> ReachabilityPlan {
    let blocked = |reason: String, action: Option<ReachAction>| {
        private_blocked(address.clone(), None, PlanBlocked { reason, action })
    };
    if facts.deployed && !facts.served_by_workers {
        return blocked(
            format!(
                "{} runs on the control plane, and a tailnet name is served by a worker, which \
                 only carries what workers serve. Deploy it to a worker first.",
                facts.environment_name
            ),
            None,
        );
    }
    if let Some((reason, action)) = machine_name_problem(facts, worker) {
        return blocked(reason, action);
    }
    let node = worker.name.as_str();
    let name = address.clone().unwrap_or_default();
    let attached = facts.attached_here.contains(&name);
    let mut warnings = vec![
        tailnet_messages::machine_name_cost(&name, node),
        format!(
            "Tailscale's certificate for `{name}` is in public certificate logs, so the name is \
             no longer secret. Only devices on your tailnet can connect to it."
        ),
    ];
    if attached
        && facts
            .audiences
            .iter()
            .any(|(a, au)| a == &name && *au == Audience::Public)
    {
        warnings.push(format!(
            "{name} is public now. This makes it private: only devices on your tailnet can open \
             it, and anyone else who uses it loses access. It needs a recent password or \
             passkey confirmation."
        ));
    }
    if !facts.deployed {
        warnings.push(format!(
            "Nothing is deployed to {} yet: it answers once a deployment runs on a worker.",
            facts.environment_name
        ));
    }
    let option = PlanOption {
        kind: PlanOptionKind::TailscaleServe,
        title: format!("Only my tailnet, at {name}"),
        description: format!(
            "{node} serves {name}, its own name in your tailnet, with Tailscale Serve: only \
             devices on your tailnet can open it, Tailscale gives it its certificate, and no DNS \
             record is added anywhere."
        ),
        actions: 1,
        recommended: true,
        way_in: format!("private:{node}"),
        steps: vec![
            step(
                StepKind::AttachDomain,
                if attached {
                    format!("{name} is already on {}", facts.environment_name)
                } else {
                    format!("Attach {name} to {}", facts.environment_name)
                },
            ),
            step(
                StepKind::SetAudience,
                format!("Make {name} private to your tailnet on {node}"),
            ),
            step(
                StepKind::ConfigureTailscaleServe,
                format!("Have {node} serve {name} with Tailscale Serve"),
            ),
            step(
                StepKind::CheckReachability,
                format!("Check {name} from {node} and from outside"),
            ),
        ],
        fields: Vec::new(),
        warnings,
        confirmation: None,
        required_permissions: permissions_for(PlanOptionKind::TailscaleServe)
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        address,
        address_error: None,
        options: vec![option],
        blocked: None,
        gate: None,
        principals: Vec::new(),
    }
}

/// The plan for the app's own name in the tailnet, a Tailscale Service
/// (ADR 065 D1 options 1, 2 and 4): tailscaled on a tagged worker serves it
/// and gets its certificate, the Service is created through the API only
/// when absent, and the tailnet admin approves the host.
fn plan_service(facts: &PlanFacts, address: Option<String>) -> ReachabilityPlan {
    let blocked = |reason: String, action: Option<ReachAction>| {
        private_blocked(address.clone(), None, PlanBlocked { reason, action })
    };
    let name = address.clone().unwrap_or_default();
    let Some((service, tailnet)) = service_of(facts, &name) else {
        return blocked(tailnet_messages::service_only(&name), None);
    };
    if facts.deployed && !facts.served_by_workers {
        return blocked(
            format!(
                "{} runs on the control plane, and a tailnet name is served by a worker, which \
                 only carries what workers serve. Deploy it to a worker first.",
                facts.environment_name
            ),
            None,
        );
    }
    let Some(worker) = service_worker(facts, &tailnet) else {
        // Say what keeps the likeliest worker from hosting it.
        let on_tailnet: Vec<&TailnetWorkerFact> = serve_candidates(facts)
            .into_iter()
            .filter(|w| w.report.tailnet.as_deref() == Some(tailnet.as_str()))
            .collect();
        return match on_tailnet.first().and_then(|w| service_host_problem(w)) {
            Some((reason, action)) => blocked(reason, action),
            None => blocked(
                format!(
                    "No worker on tailnet {tailnet} lets Temps use its Tailscale. Run `{}` on \
                     the worker that should serve {name}.",
                    temps_core::tailnet::TAILSCALE_SERVE_COMMAND
                ),
                None,
            ),
        };
    };
    if facts
        .serve_entries
        .iter()
        .any(|e| e.node_id == worker.id && e.hostname == name && e.removing)
    {
        return blocked(
            format!(
                "An earlier setup of `{name}` is still being taken off {}'s Serve config. Try \
                 again in a minute.",
                worker.name
            ),
            None,
        );
    }
    let node = worker.name.as_str();
    let tag = service_tag(worker);
    let (kind, fields) = match &facts.tailscale_credential {
        Some(credential) => match &credential.refusal {
            Some(refusal) => return blocked(refusal.clone(), None),
            None => (PlanOptionKind::TailscaleService, Vec::new()),
        },
        None => (
            PlanOptionKind::TailscaleServiceWithCredential,
            TailscaleFactory::fields(),
        ),
    };
    // ADR 065 D4: create only when absent; this install's own is found.
    let found = match facts.service_seen.as_ref().filter(|(s, _)| *s == service) {
        Some((_, ServiceSeen::Unreadable(error))) => {
            return blocked(
                format!("Temps could not read `{service}` from your tailnet: {error}"),
                None,
            )
        }
        Some((_, ServiceSeen::Found(definition))) => {
            match tailscale::ownership(definition, &facts.install_id) {
                Ownership::Foreign => {
                    return blocked(tailnet_messages::service_taken(&service), None)
                }
                Ownership::Ours { .. } => {
                    if let Some(why) = definition.differs_from_fixed(&tag) {
                        return blocked(
                            format!(
                                "{why}. Temps never changes a Service, so it cannot use it as it \
                                 is: put it back, or pick another name."
                            ),
                            None,
                        );
                    }
                    true
                }
            }
        }
        Some((_, ServiceSeen::Absent)) | None => false,
    };
    let attached = facts.attached_here.contains(&name);
    let mut warnings = vec![
        format!(
            "The tailnet admin approves {node} as the host of `{service}`, at \
             login.tailscale.com/admin/services, or once for every setup with this policy entry: \
             {}. Temps waits for it and never approves hosts itself.",
            tailscale::auto_approvers_snippet(&service, &tag)
        ),
        format!(
            "Tailscale's certificate for `{name}` is in public certificate logs, so the name is \
             no longer secret. Only devices on your tailnet can connect to it."
        ),
    ];
    if kind == PlanOptionKind::TailscaleServiceWithCredential {
        warnings.push(
            "The OAuth client's `services` scope is tailnet-wide: whoever holds its secret can \
             create, change and delete every Service in your tailnet. Temps seals it, never shows \
             it again, and asks for `services` and `devices:core:read` only."
                .to_string(),
        );
    }
    if attached
        && facts
            .audiences
            .iter()
            .any(|(a, au)| a == &name && *au == Audience::Public)
    {
        warnings.push(format!(
            "{name} is public now. This makes it private: only devices on your tailnet can open \
             it, and anyone else who uses it loses access. It needs a recent password or \
             passkey confirmation."
        ));
    }
    if !facts.deployed {
        warnings.push(format!(
            "Nothing is deployed to {} yet: it answers once a deployment runs on a worker.",
            facts.environment_name
        ));
    }
    let mut steps = Vec::new();
    if kind == PlanOptionKind::TailscaleServiceWithCredential {
        steps.push(step(
            StepKind::SaveTailscaleCredential,
            "Check the Tailscale OAuth client's scopes and save it",
        ));
    }
    steps.extend([
        step(
            StepKind::AttachDomain,
            if attached {
                format!("{name} is already on {}", facts.environment_name)
            } else {
                format!("Attach {name} to {}", facts.environment_name)
            },
        ),
        step(
            StepKind::SetAudience,
            format!("Make {name} private to your tailnet on {node}"),
        ),
        step(
            StepKind::DefineTailscaleService,
            if found {
                format!("Use `{service}`, which this Temps install made earlier")
            } else {
                format!("Create `{service}` in your tailnet, for hosts tagged {tag}")
            },
        ),
        step(
            StepKind::ConfigureTailscaleServe,
            format!("Have {node} serve and advertise `{service}`"),
        ),
        step(
            StepKind::WaitForServiceApproval,
            format!("Wait for the tailnet admin to approve {node} as its host"),
        ),
        step(
            StepKind::CheckReachability,
            format!("Check {name} from {node} and from outside"),
        ),
    ]);
    let option = PlanOption {
        kind,
        title: format!("Only my tailnet, at {name}"),
        description: format!(
            "{node} serves {name}, this app's own name in your tailnet, as the Tailscale Service \
             `{service}`: only devices on your tailnet can open it, Tailscale gives it its \
             certificate, the name stays if the app moves to another worker, and no DNS record \
             is added anywhere."
        ),
        actions: if kind == PlanOptionKind::TailscaleServiceWithCredential {
            3
        } else {
            2
        },
        recommended: true,
        way_in: format!("private:{node}"),
        steps,
        fields,
        warnings,
        confirmation: None,
        required_permissions: permissions_for(kind)
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        address,
        address_error: None,
        options: vec![option],
        blocked: None,
        gate: None,
        principals: Vec::new(),
    }
}

/// The plan for a private address on a worker's private listener (ADR 064
/// D5): DNS in a zone Temps manages, a DNS-01 certificate.
fn plan_listener(
    facts: &PlanFacts,
    address: Option<String>,
    ask_address: bool,
) -> ReachabilityPlan {
    let blocked_plan = private_blocked;
    let block = |reason: String, action: Option<ReachAction>| PlanBlocked { reason, action };
    let shown = address.clone().unwrap_or_else(|| "the address".to_string());

    if facts.deployed && !facts.served_by_workers {
        return blocked_plan(
            address,
            None,
            block(
                format!(
                    "{} runs on the control plane, and a private address is served by a \
                     worker's private listener, which only carries what workers serve. Deploy \
                     it to a worker first.",
                    facts.environment_name
                ),
                None,
            ),
        );
    }

    let Some((worker, ip)) = private_worker(facts) else {
        // Temps cannot open a listener on someone's private network; the
        // worker's owner does, on the worker.
        let reason_action = match (
            facts.private_workers.first(),
            &facts.suggested_private_worker,
        ) {
            (Some(w), _) => (
                format!(
                    "{} is in private mode, but its private listener is not running: {}",
                    w.name,
                    w.error
                        .clone()
                        .unwrap_or_else(|| "it is not bound on a private address.".to_string())
                ),
                ReachAction::new(
                    ActionKind::RunCommand,
                    format!("On {}, as root, run", w.name),
                )
                .with_command(PRIVATE_COMMAND),
            ),
            (None, Some(name)) => (
                format!(
                    "No worker listens on your private network yet, and Temps cannot turn that \
                     on by itself: the worker's owner does. Run the command on {name}, then \
                     plan again."
                ),
                ReachAction::new(ActionKind::RunCommand, format!("On {name}, as root, run"))
                    .with_command(PRIVATE_COMMAND),
            ),
            (None, None) => (
                "A private address is served by a worker on your private network, and this \
                 cluster has no active worker. Add one with `--ingress private --listen \
                 tailscale` on its join command."
                    .to_string(),
                ReachAction::new(ActionKind::OpenIngress, "Add a worker")
                    .with_href("/settings/nodes"),
            ),
        };
        return blocked_plan(address, None, block(reason_action.0, Some(reason_action.1)));
    };
    // ADR 065 D1: Serve on 443 of the same Tailscale address would take
    // the listener's traffic.
    if worker.tailscale {
        if let Some(entry) = facts.serve_entries.iter().find(|e| {
            e.node_id == worker.id
                && e.via == temps_entities::audience::via::TAILSCALE_SERVE
                && !e.removing
        }) {
            return blocked_plan(
                address,
                None,
                block(
                    tailnet_messages::serve_exclusive(
                        &worker.name,
                        &entry.hostname,
                        &ip.to_string(),
                    ),
                    None,
                ),
            );
        }
    }
    let range = temps_core::worker_ingress::private_block(ip).unwrap_or("its private network");
    let network = if worker.tailscale {
        "Tailscale".to_string()
    } else {
        range.to_string()
    };

    let Some(zone) = address
        .as_deref()
        .and_then(|host| zone_for(host, &facts.zones))
        .filter(|z| z.verified)
    else {
        let zones: Vec<String> = facts
            .zones
            .iter()
            .filter(|z| z.verified)
            .map(|z| z.domain.clone())
            .collect();
        return blocked_plan(
            address,
            None,
            block(
                format!(
                    "Temps points {shown} at {}'s private address and gets its certificate over \
                     DNS-01, and both need its zone managed by a DNS provider in Temps. Add \
                     the zone under DNS providers{}.",
                    worker.name,
                    if zones.is_empty() {
                        String::new()
                    } else {
                        format!(", or use a hostname under {}", zones.join(", "))
                    }
                ),
                Some(
                    ReachAction::new(ActionKind::OpenIngress, "Add a DNS provider")
                        .with_href("/dns-providers"),
                ),
            ),
        );
    };
    let host = address.clone().unwrap_or_default();
    let record_type = record_type_of(ip);
    let existing = planned_record(facts, &host, record_type);
    match &existing {
        PlannedRecord::Present { record } if !record.managed => {
            return blocked_plan(
                address,
                None,
                block(
                    format!(
                        "{shown} already has an {} record ({}) at {} that Temps did not create. \
                         Temps never changes a record it does not manage: remove it there, or \
                         use another hostname.",
                        record.record_type, record.content, zone.provider_name
                    ),
                    None,
                ),
            )
        }
        PlannedRecord::Unknown => {
            return blocked_plan(
                address,
                None,
                block(
                    format!(
                        "Temps could not read {shown}'s DNS records at {} just now, so it cannot \
                         show what would change. Try again in a minute.",
                        zone.provider_name
                    ),
                    None,
                ),
            )
        }
        _ => {}
    }
    let attached = facts.attached_here.contains(&host);
    let attach_label = if attached {
        format!("{shown} is already on {}", facts.environment_name)
    } else {
        format!("Attach {shown} to {}", facts.environment_name)
    };
    let dns_label = match &existing {
        PlannedRecord::Present { record } if record.content == ip.to_string() => {
            format!("{shown} already points at {ip} in {}", zone.domain)
        }
        PlannedRecord::Present { record } => format!(
            "Change {shown} from {} to {ip} in {}",
            record.content, zone.domain
        ),
        _ => format!(
            "Point {shown} at {ip} ({}'s private address) in {}",
            worker.name, zone.domain
        ),
    };
    let mut warnings = vec![format!(
        "This adds a public DNS record: anyone can look up {shown} and see that it exists and \
         that it points at {ip}, an address inside your network. Nobody outside {range} can \
         connect to it, but the name and the internal address are no longer secret."
    )];
    if let PlannedRecord::Present { record } = &existing {
        if record.content != ip.to_string() {
            warnings.push(format!(
                "{shown} points at {} now; this changes it to {ip}. Undo puts {} back while \
                 nobody else has changed it meanwhile.",
                record.content, record.content
            ));
        }
    }
    if attached
        && facts
            .audiences
            .iter()
            .any(|(a, au)| a == &host && *au == Audience::Public)
    {
        warnings.push(format!(
            "{shown} is public now. This makes it private: only devices on {network} can open \
             it, and anyone else who uses it loses access. It needs a recent password or \
             passkey confirmation."
        ));
    }
    if !facts.deployed {
        warnings.push(format!(
            "Nothing is deployed to {} yet: it answers once a deployment runs on a worker.",
            facts.environment_name
        ));
    }
    let mut fields = Vec::new();
    if ask_address {
        fields.push(address_field(address.as_deref()));
    }
    let option = PlanOption {
        kind: PlanOptionKind::PrivateWorker,
        title: format!("Only my private network, through {}", worker.name),
        description: format!(
            "{} answers {shown} on its private address {ip} ({network}); only devices in \
             {range} can connect. Temps points {shown} there through {} and issues its \
             certificate over DNS-01, which needs nothing reachable from the internet.",
            worker.name, zone.provider_name
        ),
        actions: 1,
        recommended: true,
        way_in: format!("private:{}", worker.name),
        steps: vec![
            step(StepKind::AttachDomain, attach_label),
            step(
                StepKind::SetAudience,
                format!("Make {shown} private to {range} on {}", worker.name),
            ),
            step(StepKind::SetDnsRecord, dns_label),
            step(
                StepKind::IssueCertificate,
                format!("Issue a certificate for {shown} over DNS-01"),
            ),
            step(
                StepKind::CheckReachability,
                format!("Check {shown} from {} and from outside", worker.name),
            ),
        ],
        fields,
        warnings,
        confirmation: None,
        required_permissions: permissions_for(PlanOptionKind::PrivateWorker)
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        gate: None,
        address,
        address_error: None,
        options: vec![option],
        blocked: None,
        principals: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunnel(name: &str, domain: &str, zone: &str) -> TunnelFact {
        TunnelFact {
            id: 1,
            name: name.into(),
            provider: "cloudflare_tunnel".into(),
            dns_provider_id: Some(3),
            domain: domain.into(),
            zone: Some(zone.into()),
            ready: true,
            has_nodes: true,
            auto_publish: true,
            environment_hosts: false,
        }
    }

    fn zone(domain: &str, provider_type: &str) -> ZoneFact {
        ZoneFact {
            provider_id: 3,
            provider_name: "cf".into(),
            provider_type: provider_type.into(),
            domain: domain.into(),
            verified: true,
            auto_manage: true,
        }
    }

    fn direct(takes_port_80: bool) -> DirectIngress {
        DirectIngress {
            name: "edge-1".into(),
            node_id: Some(9),
            ips: vec!["203.0.113.5".parse().expect("ip")],
            takes_port_80,
            check_failed: None,
        }
    }

    fn worker() -> NamedId {
        NamedId {
            id: 2,
            name: "master".into(),
        }
    }

    fn base() -> PlanFacts {
        PlanFacts {
            audience: Audience::Public,
            project_slug: "myapp".into(),
            environment_slug: "production".into(),
            environment_name: "production".into(),
            deployed: true,
            served_by_workers: true,
            ..Default::default()
        }
    }

    fn kinds(plan: &ReachabilityPlan) -> Vec<PlanOptionKind> {
        plan.options.iter().map(|o| o.kind).collect()
    }

    /// ADR 065 D6: an address whose audience this version cannot serve is
    /// never changed by a run, in either direction.
    #[test]
    fn an_address_with_an_audience_this_version_cannot_serve_is_left_alone() {
        for requested in [Audience::Public, Audience::Private] {
            let facts = PlanFacts {
                audience: requested,
                requested: Some("app.example.com".into()),
                attached_here: vec!["app.example.com".into()],
                audiences: vec![(
                    "app.example.com".into(),
                    Audience::parse("a_future_audience"),
                )],
                zones: vec![zone("example.com", "cloudflare")],
                ..base()
            };
            let plan = plan(&facts);
            assert!(plan.options.is_empty(), "{requested:?}");
            let blocked = plan.blocked.expect("blocked");
            assert!(
                blocked.reason.contains("newer version"),
                "{}",
                blocked.reason
            );
        }
    }

    /// ADR 065 D7: a signed-in plan is a public plan with the sign-in put
    /// in front of the address right after it is attached, and the
    /// optional list of who may sign in.
    #[test]
    fn a_signed_in_plan_puts_the_sign_in_in_front_after_attaching() {
        let facts = PlanFacts {
            audience: Audience::SignedIn,
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            directs: vec![direct(true)],
            ..base()
        };
        let plan = plan(&facts);
        assert!(plan.blocked.is_none(), "{:?}", plan.blocked);
        assert!(!plan.options.is_empty());
        for option in &plan.options {
            let kinds: Vec<StepKind> = option.steps.iter().map(|s| s.kind).collect();
            let gate = kinds
                .iter()
                .position(|k| *k == StepKind::SetAudience)
                .expect("a SetAudience step");
            if let Some(attach) = kinds.iter().position(|k| *k == StepKind::AttachDomain) {
                assert_eq!(gate, attach + 1, "{kinds:?}");
            }
            let keys: Vec<&str> = option.fields.iter().map(|f| f.key.as_str()).collect();
            assert!(keys.contains(&FIELD_GATE_TEAMS) && keys.contains(&FIELD_GATE_USERS));
            assert!(option
                .fields
                .iter()
                .filter(|f| f.key == FIELD_GATE_TEAMS || f.key == FIELD_GATE_USERS)
                .all(|f| !f.required));
            assert!(option.warnings[0].contains("Only people who sign in"));
            assert!(!option
                .warnings
                .iter()
                .any(|w| w.contains("password protection")));
        }
    }

    /// The sign-in is per address: a plan never implies the app is behind
    /// it while the generated hostname or a Compose service name still
    /// opens it for anyone.
    #[test]
    fn a_signed_in_plan_names_every_address_that_stays_open() {
        let facts = PlanFacts {
            audience: Audience::SignedIn,
            requested: Some("app.example.com".into()),
            attached_here: vec!["app.example.com".into()],
            audiences: vec![
                ("app.example.com".into(), Audience::Public),
                ("shop-prod.apps.example.net".into(), Audience::Public),
                ("lan.example.com".into(), Audience::Private),
                ("team.example.com".into(), Audience::SignedIn),
            ],
            service_addresses: vec!["api--shop-prod.apps.example.net".into()],
            zones: vec![zone("example.com", "cloudflare")],
            directs: vec![direct(true)],
            ..base()
        };
        let plan = plan(&facts);
        let option = plan.options.first().expect("an option");
        let warning = option
            .warnings
            .iter()
            .find(|w| w.contains("stays open to anyone"))
            .unwrap_or_else(|| panic!("{:?}", option.warnings));
        assert!(
            warning.contains("api--shop-prod.apps.example.net, shop-prod.apps.example.net"),
            "{warning}"
        );
        // Not the address being signed in, nor ones that are not public.
        assert!(!warning.contains(" app.example.com,"), "{warning}");
        assert!(!warning.contains("lan.example.com"), "{warning}");
        assert!(!warning.contains("team.example.com"), "{warning}");

        let alone = PlanFacts {
            audiences: vec![("app.example.com".into(), Audience::Public)],
            service_addresses: vec![],
            ..facts
        };
        let plan = super::plan(&alone);
        let option = plan.options.first().expect("an option");
        assert!(
            !option.warnings.iter().any(|w| w.contains("stays open")),
            "{:?}",
            option.warnings
        );
    }

    #[test]
    fn a_signed_in_plan_says_password_protection_also_applies() {
        let facts = PlanFacts {
            audience: Audience::SignedIn,
            password_protected: true,
            requested: Some("app.example.com".into()),
            attached_here: vec!["app.example.com".into()],
            audiences: vec![("app.example.com".into(), Audience::Private)],
            zones: vec![zone("example.com", "cloudflare")],
            directs: vec![direct(true)],
            ..base()
        };
        let plan = plan(&facts);
        let option = plan.options.first().expect("an option");
        assert!(
            option.warnings.iter().any(|w| w.contains("Both apply")),
            "{:?}",
            option.warnings
        );
        assert!(
            option.warnings.iter().any(|w| w.contains("is private now")),
            "{:?}",
            option.warnings
        );
        // Signed in is not public: no "this makes it public" warning.
        assert!(!option
            .warnings
            .iter()
            .any(|w| w.contains("This makes it public")));
    }

    #[test]
    fn gate_list_answers_take_positive_ids_and_drop_an_empty_list() {
        use serde_json::json;
        let answers = |teams: serde_json::Value, users: serde_json::Value| {
            std::collections::BTreeMap::from([
                (FIELD_GATE_TEAMS.to_string(), teams),
                (FIELD_GATE_USERS.to_string(), users),
            ])
        };
        assert_eq!(
            gate_allow_answer(&std::collections::BTreeMap::new()),
            Ok(None)
        );
        assert_eq!(
            gate_allow_answer(&answers(json!([]), json!(null))),
            Ok(None)
        );
        let list = gate_allow_answer(&answers(json!([3, "1", 3]), json!(["7"])))
            .expect("valid")
            .expect("a list");
        assert_eq!(list.teams, vec![1, 3]);
        assert_eq!(list.users, vec![7]);
        for bad in [
            json!([0]),
            json!([-2]),
            json!(["x"]),
            json!("1,2"),
            json!([1.5]),
        ] {
            assert!(
                gate_allow_answer(&answers(bad.clone(), json!([]))).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn an_existing_tunnel_wins_and_suggests_an_address_under_it() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            cloudflare_accounts: vec![NamedId {
                id: 3,
                name: "cf".into(),
            }],
            tunnel_workers: vec![worker()],
            directs: vec![direct(true)],
            ..base()
        };
        let plan = plan(&facts);
        assert_eq!(plan.address.as_deref(), Some("myapp.example.com"));
        // A covering tunnel makes a new one pointless; direct comes second.
        assert_eq!(
            kinds(&plan),
            vec![
                PlanOptionKind::ExistingTunnel,
                PlanOptionKind::DirectManagedDns
            ]
        );
        let first = &plan.options[0];
        assert!(first.recommended && !plan.options[1].recommended);
        assert_eq!(first.actions, 1);
        assert_eq!(first.way_in, "tunnel:homelab");
        // Automatic publishing: no hostname choice to make.
        assert!(!first
            .steps
            .iter()
            .any(|s| s.kind == StepKind::RouteHostname));
        assert_eq!(first.fields[0].key, FIELD_ADDRESS);
    }

    /// ADR 070 D1/D2: a tunnel option shows the HTTPS redirect and the
    /// port rule, for this hostname only, before the address is attached,
    /// and says which token permissions they need. Direct options do not.
    #[test]
    fn a_tunnel_plan_shows_the_https_redirect_and_the_port_rule() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            directs: vec![direct(true)],
            ..base()
        };
        let plan = plan(&facts);
        let tunneled = &plan.options[0];
        assert_eq!(tunneled.kind, PlanOptionKind::ExistingTunnel);
        assert_eq!(
            tunneled.steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![
                StepKind::RedirectToHttps,
                StepKind::StandardPortsOnly,
                StepKind::AttachDomain,
                StepKind::SetHttpPolicy,
                StepKind::WaitForPublication,
                StepKind::CheckReachability,
            ]
        );
        // HTTPS only is the default, offered with the two ways off it.
        let field = tunneled
            .fields
            .iter()
            .find(|f| f.key == FIELD_HTTP_POLICY)
            .expect("the choice");
        assert!(!field.required);
        assert_eq!(field.placeholder.as_deref(), Some("https_only"));
        let FieldKind::Choice { choices } = &field.kind else {
            panic!("a choice field");
        };
        assert_eq!(
            choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
            vec!["https_only", "allow_http", "allow_all"]
        );
        assert_eq!(choices[0].label, "HTTPS only (recommended)");
        assert!(!tunneled
            .warnings
            .iter()
            .any(|w| w.contains("unencrypted HTTP")));
        assert_eq!(
            tunneled.steps[0].label,
            "Redirect http://myapp.example.com to HTTPS, with a Cloudflare redirect rule for \
             this hostname only"
        );
        assert!(tunneled.steps[1]
            .label
            .starts_with("Refuse myapp.example.com on Cloudflare's other ports (8080, 8443"));
        assert!(tunneled.warnings.iter().any(|w| w
            .contains("the token needs Zone · Single Redirect · Edit")
            && w.contains("Zone · Zone WAF · Edit")
            && w.contains("still publishes")));
        let direct = &plan.options[1];
        assert_eq!(direct.kind, PlanOptionKind::DirectManagedDns);
        assert!(!direct.steps.iter().any(|s| matches!(
            s.kind,
            StepKind::RedirectToHttps | StepKind::StandardPortsOnly
        )));
    }

    /// ADR 070 D1: a new tunnel adds the rules once it exists, right
    /// before the address is attached.
    #[test]
    fn a_new_tunnel_plan_adds_the_rules_before_attaching() {
        let facts = PlanFacts {
            requested: Some("myapp.example.com".into()),
            tunnel_workers: vec![worker()],
            ..base()
        };
        let plan = plan(&facts);
        assert_eq!(kinds(&plan), vec![PlanOptionKind::NewTunnelWithToken]);
        let kinds: Vec<StepKind> = plan.options[0].steps.iter().map(|s| s.kind).collect();
        let at = kinds
            .iter()
            .position(|k| *k == StepKind::RedirectToHttps)
            .expect("redirect");
        assert_eq!(
            &kinds[at - 1..at + 3],
            &[
                StepKind::WaitForTunnel,
                StepKind::RedirectToHttps,
                StepKind::StandardPortsOnly,
                StepKind::AttachDomain
            ]
        );
        // The token help names the optional permissions.
        let token = plan.options[0]
            .fields
            .iter()
            .find(|f| f.key == FIELD_TOKEN)
            .expect("token field");
        assert!(token
            .help
            .as_deref()
            .is_some_and(|h| h.contains("Zone · Single Redirect · Edit")));
    }

    /// ADR 070 D1: no redirect for a hostname the free certificate does
    /// not cover; the port rule stays, and the plan says why.
    #[test]
    fn a_hostname_below_the_free_certificate_gets_no_redirect() {
        let facts = PlanFacts {
            requested: Some("myapp.lab.example.com".into()),
            tunnels: vec![tunnel("homelab", "lab.example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            ..base()
        };
        let plan = plan(&facts);
        let option = &plan.options[0];
        assert_eq!(option.kind, PlanOptionKind::ExistingTunnel);
        let kinds: Vec<StepKind> = option.steps.iter().map(|s| s.kind).collect();
        assert!(!kinds.contains(&StepKind::RedirectToHttps));
        assert_eq!(kinds[0], StepKind::StandardPortsOnly);
        assert!(option
            .warnings
            .iter()
            .any(|w| w.starts_with("HTTPS only is not offered for myapp.lab.example.com")));
        let (allowed, default) = http_policy_choices(&facts, "myapp.lab.example.com");
        assert_eq!(allowed, vec![HttpPolicy::AllowHttp, HttpPolicy::AllowAll]);
        assert_eq!(default, HttpPolicy::AllowHttp);
    }

    fn policy_steps(policy: HttpPolicy) -> Vec<StepKind> {
        let mut steps = vec![
            step(StepKind::AttachDomain, "attach"),
            step(StepKind::WaitForPublication, "wait"),
            step(StepKind::CheckReachability, "check"),
        ];
        place_http_policy_steps(&mut steps, "myapp.example.com", policy);
        steps.iter().map(|s| s.kind).collect()
    }

    /// ADR 070 D1: each choice runs only the rules it wants, and always
    /// records itself right after the address is attached.
    #[test]
    fn each_choice_runs_only_its_rules() {
        assert_eq!(
            policy_steps(HttpPolicy::HttpsOnly),
            vec![
                StepKind::RedirectToHttps,
                StepKind::StandardPortsOnly,
                StepKind::AttachDomain,
                StepKind::SetHttpPolicy,
                StepKind::WaitForPublication,
                StepKind::CheckReachability,
            ]
        );
        assert_eq!(
            policy_steps(HttpPolicy::AllowHttp),
            vec![
                StepKind::StandardPortsOnly,
                StepKind::AttachDomain,
                StepKind::SetHttpPolicy,
                StepKind::WaitForPublication,
                StepKind::CheckReachability,
            ]
        );
        assert_eq!(
            policy_steps(HttpPolicy::AllowAll),
            vec![
                StepKind::AttachDomain,
                StepKind::SetHttpPolicy,
                StepKind::WaitForPublication,
                StepKind::CheckReachability,
            ]
        );
        // Placing again replaces, never duplicates.
        let mut steps = vec![step(StepKind::AttachDomain, "attach")];
        place_http_policy_steps(&mut steps, "myapp.example.com", HttpPolicy::HttpsOnly);
        place_http_policy_steps(&mut steps, "myapp.example.com", HttpPolicy::AllowAll);
        assert_eq!(
            steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![StepKind::AttachDomain, StepKind::SetHttpPolicy]
        );
    }

    /// Applying a choice puts the policy steps where the plan showed them:
    /// the rules ahead of the Access app, the record after the audience.
    #[test]
    fn placing_a_choice_keeps_the_access_app_and_audience_where_the_plan_put_them() {
        let mut steps = vec![
            step(StepKind::AccessApp, "app"),
            step(StepKind::AttachDomain, "attach"),
            step(StepKind::SetAudience, "gate"),
            step(StepKind::CheckReachability, "check"),
        ];
        place_http_policy_steps(&mut steps, "myapp.example.com", HttpPolicy::HttpsOnly);
        let expected = vec![
            StepKind::RedirectToHttps,
            StepKind::StandardPortsOnly,
            StepKind::AccessApp,
            StepKind::AttachDomain,
            StepKind::SetAudience,
            StepKind::SetHttpPolicy,
            StepKind::CheckReachability,
        ];
        assert_eq!(steps.iter().map(|s| s.kind).collect::<Vec<_>>(), expected);
        place_http_policy_steps(&mut steps, "myapp.example.com", HttpPolicy::HttpsOnly);
        assert_eq!(steps.iter().map(|s| s.kind).collect::<Vec<_>>(), expected);

        // Every option of a signed-in Access plan keeps its steps as shown
        // when its default choice is applied.
        let plan = plan(&access_facts(vec![provider(4, Ok(vec![]))]));
        assert!(!plan.options.is_empty());
        for option in &plan.options {
            let policy = chosen_http_policy(option, None)
                .expect("a valid default")
                .expect("a tunnel option offers the choice");
            let mut applied = option.steps.clone();
            place_http_policy_steps(&mut applied, "myapp.example.com", policy);
            assert_eq!(
                applied.iter().map(|s| s.kind).collect::<Vec<_>>(),
                option.steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
                "{}",
                option.way_in
            );
        }
    }

    /// A zone Temps only guesses never takes HTTPS only away: the last two
    /// labels of `myapp.example.co.uk` are `co.uk`, not its zone.
    #[test]
    fn a_guessed_zone_does_not_refuse_https_only() {
        let (allowed, default) = http_policy_choices(&base(), "myapp.example.co.uk");
        assert_eq!(allowed, HttpPolicy::ALL.to_vec());
        assert_eq!(default, HttpPolicy::HttpsOnly);
        // A zone Temps knows still decides.
        let facts = PlanFacts {
            zones: vec![zone("example.co.uk", "cloudflare")],
            ..base()
        };
        let (allowed, _) = http_policy_choices(&facts, "myapp.lab.example.co.uk");
        assert!(!allowed.contains(&HttpPolicy::HttpsOnly));
        let (allowed, _) = http_policy_choices(&facts, "myapp.example.co.uk");
        assert!(allowed.contains(&HttpPolicy::HttpsOnly));
    }

    /// ADR 070 D1: a re-run offers the choice the address keeps, and the
    /// plan says plainly what it will also answer.
    #[test]
    fn a_re_run_keeps_the_recorded_choice_and_states_the_exposure() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            http_policies: [("myapp.example.com".to_string(), HttpPolicy::AllowAll)]
                .into_iter()
                .collect(),
            ..base()
        };
        let plan = plan(&facts);
        let option = &plan.options[0];
        assert_eq!(option.kind, PlanOptionKind::ExistingTunnel);
        let kinds: Vec<StepKind> = option.steps.iter().map(|s| s.kind).collect();
        assert!(!kinds.contains(&StepKind::RedirectToHttps));
        assert!(!kinds.contains(&StepKind::StandardPortsOnly));
        assert!(kinds.contains(&StepKind::SetHttpPolicy));
        let field = option
            .fields
            .iter()
            .find(|f| f.key == FIELD_HTTP_POLICY)
            .expect("the choice");
        assert_eq!(field.placeholder.as_deref(), Some("allow_all"));
        assert!(option.warnings.iter().any(|w| w.contains(
            "myapp.example.com will also answer over unencrypted HTTP (http://myapp.example.com) \
             and on Cloudflare's other ports"
        )));
        // No rule, so no word about the permissions rules need.
        assert!(!option
            .warnings
            .iter()
            .any(|w| w.contains("Zone · Zone WAF · Edit")));
    }

    /// ADR 070 D1: applying takes the answer, else the default shown;
    /// a value the option does not offer is refused.
    #[test]
    fn applying_takes_the_answer_or_the_default_shown() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            directs: vec![direct(true)],
            ..base()
        };
        let plan = plan(&facts);
        let tunneled = &plan.options[0];
        assert_eq!(
            chosen_http_policy(tunneled, None),
            Ok(Some(HttpPolicy::HttpsOnly))
        );
        assert_eq!(
            chosen_http_policy(tunneled, Some("allow_http")),
            Ok(Some(HttpPolicy::AllowHttp))
        );
        let refused = chosen_http_policy(tunneled, Some("sometimes")).expect_err("refused");
        assert!(
            refused.contains("`https_only`, `allow_http`, `allow_all`"),
            "{refused}"
        );
        // A direct way in offers no choice and creates no rule.
        let direct = &plan.options[1];
        assert_eq!(direct.kind, PlanOptionKind::DirectManagedDns);
        assert_eq!(chosen_http_policy(direct, Some("allow_all")), Ok(None));
    }

    /// ADR 068 D5's tunnel fix: the environment's own host under a tunnel
    /// is offered first as "every environment gets one", behind the
    /// public-names warning; the single-host option stays as the alternative.
    #[test]
    fn the_environment_host_under_a_tunnel_offers_environment_hosts_first() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            requested: Some("myapp-production.example.com".into()),
            environment_host_label: Some("myapp-production".into()),
            ..base()
        };
        let plan = plan(&facts);
        assert_eq!(
            kinds(&plan),
            vec![
                PlanOptionKind::TunnelEnvironmentHosts,
                PlanOptionKind::ExistingTunnel
            ]
        );
        let first = &plan.options[0];
        assert_eq!(first.way_in, "tunnel:homelab");
        assert_eq!(
            first.confirmation.as_deref(),
            Some(ENVIRONMENT_HOSTS_WARNING)
        );
        assert_eq!(
            first.steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![
                StepKind::PublishEnvironmentHosts,
                StepKind::WaitForPublication,
                StepKind::CheckReachability
            ]
        );
        assert_eq!(
            permissions_for(PlanOptionKind::TunnelEnvironmentHosts),
            vec![Permission::SettingsWrite]
        );
    }

    #[test]
    fn environment_hosts_are_not_offered_when_they_cannot_apply() {
        let with = |facts: PlanFacts| {
            kinds(&plan(&PlanFacts {
                requested: Some("myapp-production.example.com".into()),
                environment_host_label: Some("myapp-production".into()),
                ..facts
            }))
        };
        let homelab = || tunnel("homelab", "example.com", "example.com");
        // Another address than the environment's own host.
        assert!(!kinds(&plan(&PlanFacts {
            tunnels: vec![homelab()],
            requested: Some("shop.example.com".into()),
            environment_host_label: Some("myapp-production".into()),
            ..base()
        }))
        .contains(&PlanOptionKind::TunnelEnvironmentHosts));
        // Some tunnel already has them on (Q3: at most one).
        assert!(!with(PlanFacts {
            tunnels: vec![
                homelab(),
                TunnelFact {
                    id: 4,
                    environment_hosts: true,
                    ..tunnel("edge", "other.example.net", "example.net")
                },
            ],
            ..base()
        })
        .contains(&PlanOptionKind::TunnelEnvironmentHosts));
        // An address that is private or for signed-in people (Security).
        assert!(!with(PlanFacts {
            tunnels: vec![homelab()],
            audiences: vec![("myapp.internal.example.com".into(), Audience::Private)],
            ..base()
        })
        .contains(&PlanOptionKind::TunnelEnvironmentHosts));
        // A plan to make the address signed-in (ADR 065): environment hosts
        // are public.
        assert!(!with(PlanFacts {
            tunnels: vec![homelab()],
            audience: Audience::SignedIn,
            ..base()
        })
        .contains(&PlanOptionKind::TunnelEnvironmentHosts));
        // Served by the control plane: no tunnel carries it.
        assert!(!with(PlanFacts {
            tunnels: vec![homelab()],
            served_by_workers: false,
            ..base()
        })
        .contains(&PlanOptionKind::TunnelEnvironmentHosts));
    }

    #[test]
    fn a_tunnel_that_publishes_only_chosen_hostnames_gets_a_choice() {
        let facts = PlanFacts {
            tunnels: vec![TunnelFact {
                auto_publish: false,
                ..tunnel("homelab", "example.com", "example.com")
            }],
            requested: Some("Shop.Example.com.".into()),
            ..base()
        };
        let plan = plan(&facts);
        assert_eq!(plan.address.as_deref(), Some("shop.example.com"));
        assert!(plan.options[0]
            .steps
            .iter()
            .any(|s| s.kind == StepKind::RouteHostname));
        // The address was given, so it is not asked again; only what it
        // answers is (ADR 070).
        assert!(plan.options[0]
            .fields
            .iter()
            .all(|f| f.key == FIELD_HTTP_POLICY));
    }

    #[test]
    fn a_cloudflare_account_without_a_tunnel_offers_a_new_tunnel_on_a_worker() {
        let facts = PlanFacts {
            zones: vec![zone("example.com", "cloudflare")],
            cloudflare_accounts: vec![NamedId {
                id: 3,
                name: "cf".into(),
            }],
            tunnel_workers: vec![worker()],
            ..base()
        };
        let plan = plan(&facts);
        assert_eq!(kinds(&plan), vec![PlanOptionKind::NewTunnel]);
        let option = &plan.options[0];
        assert_eq!(option.actions, 2);
        assert_eq!(option.title, "Create a tunnel on master");
        // One account: nothing to choose.
        assert!(option.fields.iter().all(|f| f.key != FIELD_DNS_PROVIDER));
    }

    #[test]
    fn nothing_but_a_worker_asks_for_a_token() {
        let facts = PlanFacts {
            tunnel_workers: vec![worker()],
            requested: Some("myapp.example.com".into()),
            ..base()
        };
        let plan = plan(&facts);
        assert_eq!(kinds(&plan), vec![PlanOptionKind::NewTunnelWithToken]);
        let option = &plan.options[0];
        assert_eq!(option.actions, 3);
        assert_eq!(option.fields.len(), 2);
        assert_eq!(option.fields[0].key, FIELD_TOKEN);
        assert_eq!(option.fields[1].key, FIELD_HTTP_POLICY);
        assert!(matches!(option.fields[0].kind, FieldKind::Secret));
        assert_eq!(option.steps[0].kind, StepKind::CheckToken);
    }

    #[test]
    fn http_01_is_only_offered_when_a_direct_ingress_takes_port_80() {
        let facts = PlanFacts {
            directs: vec![direct(false)],
            requested: Some("myapp.example.com".into()),
            ..base()
        };
        let plan = plan(&facts);
        assert!(plan.options.is_empty());
        let blocked = plan.blocked.expect("blocked");
        assert!(blocked.reason.contains("port 80"), "{}", blocked.reason);

        let facts = PlanFacts {
            directs: vec![direct(true)],
            requested: Some("myapp.example.com".into()),
            ..base()
        };
        assert_eq!(
            kinds(&plan_of(&facts)),
            vec![PlanOptionKind::DirectManualDns]
        );
    }

    fn plan_of(facts: &PlanFacts) -> ReachabilityPlan {
        plan(facts)
    }

    #[test]
    fn direct_with_managed_dns_comes_before_a_new_tunnel_and_manual_dns_after() {
        let facts = PlanFacts {
            directs: vec![direct(true)],
            zones: vec![zone("example.com", "cloudflare")],
            cloudflare_accounts: vec![NamedId {
                id: 3,
                name: "cf".into(),
            }],
            tunnel_workers: vec![worker()],
            ..base()
        };
        assert_eq!(
            kinds(&plan(&facts)),
            vec![PlanOptionKind::DirectManagedDns, PlanOptionKind::NewTunnel]
        );

        let facts = PlanFacts {
            directs: vec![direct(true)],
            tunnel_workers: vec![worker()],
            requested: Some("myapp.example.org".into()),
            ..base()
        };
        assert_eq!(
            kinds(&plan(&facts)),
            vec![
                PlanOptionKind::NewTunnelWithToken,
                PlanOptionKind::DirectManualDns
            ]
        );
    }

    #[test]
    fn an_environment_on_the_control_plane_gets_no_tunnel() {
        let facts = PlanFacts {
            tunnel_workers: vec![worker()],
            served_by_workers: false,
            requested: Some("myapp.example.com".into()),
            ..base()
        };
        let plan = plan(&facts);
        assert!(plan.options.is_empty());
        assert!(plan
            .blocked
            .expect("blocked")
            .reason
            .contains("runs on the control plane"));
    }

    #[test]
    fn no_willing_worker_names_the_command_that_fixes_it() {
        let facts = PlanFacts {
            workers_refusing: vec!["edge-1".into()],
            requested: Some("myapp.example.com".into()),
            ..base()
        };
        let blocked = plan(&facts).blocked.expect("blocked");
        assert!(blocked.reason.contains("edge-1"));
        assert_eq!(
            blocked.action.and_then(|a| a.command).as_deref(),
            Some("temps agent ingress delegated")
        );
    }

    #[test]
    fn unusable_addresses_and_audiences_are_refused_with_the_reason() {
        let local = plan(&PlanFacts {
            requested: Some("app.localho.st".into()),
            ..base()
        });
        assert!(local.address_error.is_some());
        let taken = plan(&PlanFacts {
            requested: Some("myapp.example.com".into()),
            taken_elsewhere: ["myapp.example.com".to_string()].into_iter().collect(),
            ..base()
        });
        assert!(taken
            .address_error
            .expect("taken")
            .contains("another environment"));
        let private = plan(&PlanFacts {
            audience: Audience::Private,
            ..base()
        });
        assert!(private.options.is_empty());
        assert!(private.blocked.is_some());
    }

    #[test]
    fn tunnel_domains_and_names() {
        assert_eq!(
            tunnel_domain_for("app.example.com", "example.com"),
            "example.com"
        );
        assert_eq!(
            tunnel_domain_for("app.lab.example.com", "example.com"),
            "lab.example.com"
        );
        assert_eq!(guess_zone("app.lab.example.com"), "example.com");
        assert_eq!(unique_tunnel_name("example.com", &[]), "example-com");
        assert_eq!(
            unique_tunnel_name("example.com", &["example-com".into()]),
            "example-com-2"
        );
        let url = cloudflare_token_url();
        assert!(url.contains("accountId=*&zoneId=all&name=Temps"));
        let encoded = url
            .split_once("permissionGroupKeys=")
            .and_then(|(_, rest)| rest.split_once('&'))
            .map(|(keys, _)| keys)
            .expect("keys");
        assert_eq!(
            encoded
                .replace("%5B", "[")
                .replace("%5D", "]")
                .replace("%7B", "{")
                .replace("%7D", "}")
                .replace("%22", "\"")
                .replace("%3A", ":")
                .replace("%2C", ","),
            r#"[{"key":"zone","type":"read"},{"key":"dns","type":"edit"}]"#
        );
    }

    #[test]
    fn several_cloudflare_accounts_ask_which_one() {
        let facts = PlanFacts {
            cloudflare_accounts: vec![
                NamedId {
                    id: 3,
                    name: "a".into(),
                },
                NamedId {
                    id: 4,
                    name: "b".into(),
                },
            ],
            tunnel_workers: vec![worker()],
            requested: Some("myapp.example.net".into()),
            ..base()
        };
        let plan = plan(&facts);
        assert!(plan.options[0]
            .fields
            .iter()
            .any(|f| f.key == FIELD_DNS_PROVIDER));
    }

    #[test]
    fn a_hostname_kept_off_tunnels_is_only_routed_after_confirmation() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            choices: vec![("shop.example.com".into(), None)],
            requested: Some("shop.example.com".into()),
            ..base()
        };
        let plan = plan(&facts);
        let option = &plan.options[0];
        assert_eq!(option.kind, PlanOptionKind::ExistingTunnel);
        assert_eq!(option.actions, 2);
        assert!(option
            .steps
            .iter()
            .any(|s| s.kind == StepKind::RouteHostname));
        let confirmation = option.confirmation.as_deref().expect("confirmation");
        assert!(confirmation.contains("set to no tunnel"), "{confirmation}");
    }

    #[test]
    fn a_hostname_published_automatically_needs_no_choice_or_confirmation() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            requested: Some("shop.example.com".into()),
            ..base()
        };
        let option = &plan(&facts).options[0];
        assert_eq!(option.confirmation, None);
        assert!(!option
            .steps
            .iter()
            .any(|s| s.kind == StepKind::RouteHostname));
    }

    #[test]
    fn the_service_intent_wins_over_the_local_selection() {
        let mut other = tunnel("other", "example.com", "example.com");
        other.id = 2;
        other.ready = false;
        let mut intents = HashMap::new();
        intents.insert(
            "shop.example.com".to_string(),
            PublicationIntent::Tunnel {
                connector_id: 2,
                by_choice: true,
            },
        );
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com"), other],
            intents,
            requested: Some("shop.example.com".into()),
            ..base()
        };
        let option = &plan(&facts).options[0];
        assert_eq!(option.way_in, "tunnel:homelab");
        let confirmation = option.confirmation.as_deref().expect("confirmation");
        assert!(
            confirmation.contains("set to tunnel `other` now, which is not ready"),
            "{confirmation}"
        );
    }

    #[test]
    fn a_private_allow_list_blocks_every_tunnel_option_naming_the_rule() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            tunnel_workers: vec![worker()],
            directs: vec![direct(true)],
            zones: vec![zone("example.com", "cloudflare")],
            private_rule: Some(PrivateAllowList {
                rule: "the IP access allow list of environment `production`".into(),
                cidr: "100.64.0.0/10".into(),
            }),
            requested: Some("shop.example.com".into()),
            ..base()
        };
        let planned = plan(&facts);
        assert_eq!(kinds(&planned), vec![PlanOptionKind::DirectManagedDns]);
        let note = planned.options[0].warnings.last().expect("why no tunnel");
        assert!(
            note.contains("tunnel `homelab`")
                && note.contains("100.64.0.0/10")
                && note.contains("environment `production`"),
            "{note}"
        );
        // With no direct ingress either, it is why nothing is offered.
        let planned = plan(&PlanFacts {
            directs: Vec::new(),
            ..facts
        });
        assert!(planned.options.is_empty());
        let blocked = planned.blocked.expect("blocked");
        assert!(
            blocked.reason.contains("100.64.0.0/10"),
            "{}",
            blocked.reason
        );
    }

    #[test]
    fn the_fingerprint_follows_what_the_plan_depends_on() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            ..base()
        };
        let a = fingerprint(&facts, Some("shop.example.com"));
        assert_eq!(a.len(), 64);
        assert_eq!(a, fingerprint(&facts.clone(), Some("shop.example.com")));
        assert_ne!(a, fingerprint(&facts, Some("blog.example.com")));
        let mut changed = facts.clone();
        changed.tunnels[0].auto_publish = false;
        assert_ne!(a, fingerprint(&changed, Some("shop.example.com")));
        let mut chosen = facts.clone();
        chosen.choices.push(("shop.example.com".into(), None));
        assert_ne!(a, fingerprint(&chosen, Some("shop.example.com")));
        let mut moved = facts.clone();
        moved.worker_modes.push((2, Some("behind_proxy".into())));
        assert_ne!(a, fingerprint(&moved, Some("shop.example.com")));
        let mut audiences = facts.clone();
        audiences
            .audiences
            .push(("shop.example.com".into(), Audience::Public));
        assert_ne!(a, fingerprint(&audiences, Some("shop.example.com")));
        // Order does not matter.
        let mut two = facts.clone();
        let mut second = tunnel("b", "example.net", "example.net");
        second.id = 2;
        two.tunnels.push(second);
        let mut reversed = two.clone();
        reversed.tunnels.reverse();
        assert_eq!(
            fingerprint(&two, Some("shop.example.com")),
            fingerprint(&reversed, Some("shop.example.com"))
        );
    }

    fn seen(content: &str, modified: &str, managed: bool) -> PlannedRecord {
        PlannedRecord::Present {
            record: crate::reachability::model::DnsRecordState {
                provider_id: 3,
                label: "myapp".into(),
                record_id: Some("rec-1".into()),
                record_type: "A".into(),
                content: content.into(),
                modified_on: Some(modified.into()),
                managed,
                run_id: None,
            },
        }
    }

    fn direct_facts(record: PlannedRecord) -> PlanFacts {
        PlanFacts {
            directs: vec![direct(true)],
            zones: vec![zone("example.com", "cloudflare")],
            requested: Some("myapp.example.com".into()),
            dns_records: vec![("myapp.example.com".into(), "A".into(), record)],
            ..base()
        }
    }

    /// ADR 064 D4: a DNS record edited after planning makes `apply` refuse
    /// the plan (409 with a new one), because the record is in the
    /// fingerprint: its content, and its modification time even when the
    /// content is the same again.
    #[test]
    fn a_dns_record_edited_after_planning_changes_the_fingerprint() {
        let host = Some("myapp.example.com");
        let planned = direct_facts(seen("198.51.100.1", "2026-10-24T10:00:00Z", true));
        let print = fingerprint(&planned, host);
        assert_eq!(print, fingerprint(&planned.clone(), host));
        let edited = direct_facts(seen("198.51.100.2", "2026-10-24T10:05:00Z", true));
        assert_ne!(print, fingerprint(&edited, host));
        let touched = direct_facts(seen("198.51.100.1", "2026-10-24T10:06:00Z", true));
        assert_ne!(print, fingerprint(&touched, host));
        let removed = direct_facts(PlannedRecord::Absent);
        assert_ne!(print, fingerprint(&removed, host));
        // Only the record a plan would write is read for it.
        assert_eq!(
            records_to_read(&planned, "myapp.example.com"),
            vec![("myapp.example.com".to_string(), "A".to_string())]
        );
    }

    #[test]
    fn a_plan_shows_the_record_it_would_change_and_never_takes_over_one() {
        let plan = plan(&direct_facts(seen("198.51.100.1", "t", true)));
        let option = &plan.options[0];
        assert_eq!(option.kind, PlanOptionKind::DirectManagedDns);
        let dns = option
            .steps
            .iter()
            .find(|s| s.kind == StepKind::SetDnsRecord)
            .expect("dns step");
        assert!(
            dns.label.contains("from 198.51.100.1 to 203.0.113.5"),
            "{}",
            dns.label
        );
        assert!(option.warnings[0].contains("Undo puts 198.51.100.1 back"));

        // A record Temps does not manage is never offered for change.
        let plan = plan_of(&direct_facts(seen("198.51.100.1", "t", false)));
        assert!(!kinds(&plan).contains(&PlanOptionKind::DirectManagedDns));
        let blocked = format!("{:?}", plan.blocked);
        assert!(blocked.contains("that Temps did not create"), "{blocked}");
    }

    fn private_worker_fact(error: Option<&str>) -> PrivateWorkerFact {
        PrivateWorkerFact {
            id: 2,
            name: "homelab".into(),
            listener: Some("100.101.102.103".parse().expect("ip")),
            tailscale: true,
            error: error.map(str::to_string),
        }
    }

    fn private_facts() -> PlanFacts {
        PlanFacts {
            audience: Audience::Private,
            requested: Some("internal.example.com".into()),
            zones: vec![zone("example.com", "cloudflare")],
            private_workers: vec![private_worker_fact(None)],
            suggested_private_worker: Some("homelab".into()),
            // A tunnel and a direct ingress are never what a private
            // address goes through.
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            directs: vec![direct(true)],
            ..base()
        }
    }

    /// ADR 064 D5: without a worker in private mode the plan names the
    /// command and the worker; Temps never turns it on itself.
    #[test]
    fn a_private_plan_without_a_private_worker_names_the_command_and_worker() {
        let facts = PlanFacts {
            private_workers: Vec::new(),
            ..private_facts()
        };
        let plan = plan(&facts);
        assert!(plan.options.is_empty());
        let blocked = plan.blocked.expect("blocked");
        assert!(blocked.reason.contains("homelab"), "{}", blocked.reason);
        let action = blocked.action.expect("action");
        assert_eq!(action.kind, ActionKind::RunCommand);
        assert_eq!(action.command.as_deref(), Some(PRIVATE_COMMAND));
        assert!(action.label.contains("homelab"));

        let failing = plan_of(&PlanFacts {
            private_workers: vec![private_worker_fact(Some("tailscale0 has no address"))],
            ..private_facts()
        });
        assert!(failing.options.is_empty());
        assert!(failing
            .blocked
            .expect("blocked")
            .reason
            .contains("tailscale0 has no address"));
    }

    #[test]
    fn a_private_plan_points_at_the_listener_and_issues_over_dns_01() {
        let facts = private_facts();
        let plan = plan(&facts);
        assert_eq!(kinds(&plan), vec![PlanOptionKind::PrivateWorker]);
        let option = &plan.options[0];
        assert_eq!(option.way_in, "private:homelab");
        assert_eq!(
            option.steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![
                StepKind::AttachDomain,
                StepKind::SetAudience,
                StepKind::SetDnsRecord,
                StepKind::IssueCertificate,
                StepKind::CheckReachability
            ]
        );
        assert!(option.steps[2].label.contains("100.101.102.103"));
        assert!(option.steps[3].label.contains("DNS-01"));
        assert!(option.steps[1].label.contains("100.64.0.0/10"));
        assert!(option.description.contains("Tailscale"));
        // The console shows this before Create.
        assert!(
            option.warnings[0].contains("public DNS record")
                && option.warnings[0].contains("100.101.102.103"),
            "{:?}",
            option.warnings
        );
        assert_eq!(
            records_to_read(&facts, "internal.example.com"),
            vec![("internal.example.com".to_string(), "A".to_string())]
        );

        // DNS-01 and the record need a zone Temps manages.
        let unmanaged = plan_of(&PlanFacts {
            requested: Some("internal.example.org".into()),
            ..private_facts()
        });
        assert!(unmanaged.options.is_empty());
        assert!(unmanaged
            .blocked
            .expect("blocked")
            .reason
            .contains("DNS-01"));

        // A record Temps does not manage is never taken over.
        let mut taken = private_facts();
        taken.dns_records = vec![(
            "internal.example.com".into(),
            "A".into(),
            seen("10.0.0.9", "2026-10-01", false),
        )];
        assert!(plan_of(&taken).options.is_empty());

        // Where the environment runs on the control plane, no worker can.
        let on_cp = plan_of(&PlanFacts {
            served_by_workers: false,
            ..private_facts()
        });
        assert!(on_cp.options.is_empty());

        // The fingerprint follows the listener.
        let mut moved = private_facts();
        moved.private_workers[0].listener = Some("100.101.102.104".parse().expect("ip"));
        assert_ne!(
            fingerprint(&facts, Some("internal.example.com")),
            fingerprint(&moved, Some("internal.example.com"))
        );
    }

    /// ADR 064 Security: making a private address public says what becomes
    /// public, as a step undo can reverse.
    #[test]
    fn a_public_plan_for_a_private_address_says_it_becomes_public() {
        let facts = PlanFacts {
            requested: Some("internal.example.com".into()),
            attached_here: vec!["internal.example.com".into()],
            audiences: vec![("internal.example.com".into(), Audience::Private)],
            private_ranges: vec![("internal.example.com".into(), Some("100.64.0.0/10".into()))],
            directs: vec![direct(true)],
            zones: vec![zone("example.com", "cloudflare")],
            ..base()
        };
        let plan = plan(&facts);
        assert!(!plan.options.is_empty());
        for option in &plan.options {
            assert!(
                option.steps.iter().any(|s| s.kind == StepKind::SetAudience),
                "{:?}",
                option.steps
            );
            assert!(option.warnings[0].contains("anyone on the internet can open"));
            assert!(option.warnings[0].contains("private to 100.64.0.0/10"));
        }
        // A public address gets no such step.
        let public = plan_of(&PlanFacts {
            audiences: vec![("internal.example.com".into(), Audience::Public)],
            ..facts
        });
        assert!(public
            .options
            .iter()
            .all(|o| o.steps.iter().all(|s| s.kind != StepKind::SetAudience)));
    }

    fn access_app(id: &str, name: &str) -> AccessApp {
        AccessApp {
            id: id.into(),
            name: name.into(),
            app_type: super::super::access::SELF_HOSTED.into(),
            domain: "myapp.example.com".into(),
            aud: format!("aud-{id}"),
            domains: vec!["myapp.example.com".into()],
            policies: "p".into(),
        }
    }

    fn access_facts(providers: Vec<AccessProviderFact>) -> PlanFacts {
        PlanFacts {
            audience: Audience::SignedIn,
            gate: GateKind::CloudflareAccess,
            requested: Some("myapp.example.com".into()),
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            zones: vec![zone("example.com", "cloudflare")],
            directs: vec![direct(true)],
            access_providers: providers,
            ..base()
        }
    }

    fn provider(id: i32, covering: Result<Vec<AccessApp>, String>) -> AccessProviderFact {
        AccessProviderFact {
            id,
            name: format!("Access {id}"),
            covering,
        }
    }

    /// ADR 065 D9: Access only sees tunnel traffic, so direct ways in go;
    /// the app is made before the address is attached, and the gate set
    /// right after attaching, as for the Temps sign-in.
    #[test]
    fn an_access_plan_offers_only_tunnels_and_creates_the_app_before_attaching() {
        let plan = plan(&access_facts(vec![provider(4, Ok(vec![]))]));
        assert_eq!(plan.gate, Some(GateKind::CloudflareAccess));
        assert!(plan.blocked.is_none(), "{:?}", plan.blocked);
        assert!(!plan.options.is_empty());
        for option in &plan.options {
            assert!(option.way_in.starts_with("tunnel"), "{}", option.way_in);
            let kinds: Vec<StepKind> = option.steps.iter().map(|s| s.kind).collect();
            let app = kinds
                .iter()
                .position(|k| *k == StepKind::AccessApp)
                .expect("an AccessApp step");
            let attach = kinds
                .iter()
                .position(|k| *k == StepKind::AttachDomain)
                .expect("attach");
            let gate = kinds
                .iter()
                .position(|k| *k == StepKind::SetAudience)
                .expect("gate");
            assert!(app < attach && gate == attach + 1, "{kinds:?}");
            let keys: Vec<&str> = option.fields.iter().map(|f| f.key.as_str()).collect();
            assert!(keys.contains(&FIELD_ACCESS_EMAILS));
            assert!(keys.contains(&FIELD_ACCESS_EMAIL_DOMAINS));
            // One account: nothing to pick. No Temps sign-in list.
            assert!(!keys.contains(&FIELD_ACCESS_PROVIDER));
            assert!(!keys.contains(&FIELD_GATE_TEAMS));
            assert!(option
                .warnings
                .iter()
                .any(|w| w.contains("reach Temps directly are not offered")));
        }
    }

    #[test]
    fn an_access_plan_uses_an_app_that_already_covers_the_address() {
        let plan = plan(&access_facts(vec![
            provider(4, Ok(vec![])),
            provider(5, Ok(vec![access_app("a1", "team app")])),
        ]));
        let option = &plan.options[0];
        let step = option
            .steps
            .iter()
            .find(|s| s.kind == StepKind::AccessApp)
            .expect("step");
        assert!(step.label.contains("team app"), "{}", step.label);
        assert!(option.fields.iter().all(|f| f.key != FIELD_ACCESS_EMAILS));
        assert!(option
            .warnings
            .iter()
            .any(|w| w.contains("never edits or deletes it")));
    }

    #[test]
    fn several_access_accounts_ask_which_one_creates_the_app() {
        let plan = plan(&access_facts(vec![
            provider(4, Ok(vec![])),
            provider(5, Ok(vec![])),
        ]));
        let field = plan.options[0]
            .fields
            .iter()
            .find(|f| f.key == FIELD_ACCESS_PROVIDER)
            .expect("a provider choice");
        assert!(field.required);
        assert!(matches!(&field.kind, FieldKind::Choice { choices } if choices.len() == 2));
    }

    #[test]
    fn an_access_plan_without_a_token_points_at_where_one_is_added() {
        let plan = plan(&access_facts(vec![]));
        assert!(plan.options.is_empty());
        let blocked = plan.blocked.expect("blocked");
        assert!(blocked.reason.contains("No Cloudflare Access token"));
        assert_eq!(
            blocked.action.and_then(|a| a.href).as_deref(),
            Some(ACCESS_SETUP_PATH)
        );
    }

    #[test]
    fn an_unreadable_access_account_blocks_rather_than_risk_a_second_app() {
        let plan = plan(&access_facts(vec![provider(
            4,
            Err("token revoked".into()),
        )]));
        assert!(plan.options.is_empty());
        assert!(plan
            .blocked
            .expect("blocked")
            .reason
            .contains("token revoked"));
    }

    #[test]
    fn an_access_plan_with_no_tunnel_possible_says_why() {
        let facts = PlanFacts {
            tunnels: vec![],
            zones: vec![],
            tunnel_workers: vec![],
            served_by_workers: false,
            ..access_facts(vec![provider(4, Ok(vec![]))])
        };
        let plan = plan(&facts);
        assert!(plan.options.is_empty());
        assert!(plan
            .blocked
            .expect("blocked")
            .reason
            .contains("only sees traffic that comes through a Cloudflare tunnel"));
    }

    #[test]
    fn a_new_covering_access_app_changes_the_fingerprint_and_others_hash_as_before() {
        let before = access_facts(vec![provider(4, Ok(vec![]))]);
        let after = access_facts(vec![provider(4, Ok(vec![access_app("a1", "x")]))]);
        let host = Some("myapp.example.com");
        assert_ne!(fingerprint(&before, host), fingerprint(&after, host));
        // A public plan carries no gate part at all.
        let public = PlanFacts {
            audience: Audience::Public,
            ..before.clone()
        };
        let public_other_gate = PlanFacts {
            gate: GateKind::Temps,
            access_providers: vec![],
            ..public.clone()
        };
        assert_eq!(
            fingerprint(&public, host),
            fingerprint(&public_other_gate, host)
        );
    }

    #[test]
    fn access_policy_answers_take_lists_or_text_and_need_someone() {
        let answers = |pairs: Vec<(&str, serde_json::Value)>| {
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let policy = access_policy_answer(&answers(vec![
            (FIELD_ACCESS_EMAILS, serde_json::json!(["Ana@Example.com"])),
            (
                FIELD_ACCESS_EMAIL_DOMAINS,
                serde_json::json!("@example.org, example.net"),
            ),
        ]))
        .expect("valid");
        assert_eq!(policy.emails, vec!["ana@example.com"]);
        assert_eq!(policy.email_domains, vec!["example.net", "example.org"]);
        assert!(access_policy_answer(&answers(vec![])).is_err());
        assert!(access_policy_answer(&answers(vec![(
            FIELD_ACCESS_EMAILS,
            serde_json::json!([1])
        )]))
        .is_err());
    }

    fn tailnet_report() -> TailscaleReport {
        TailscaleReport {
            consent: true,
            root: true,
            state: temps_core::tailnet::TailscaleState::Ready,
            version: Some("1.94.1".into()),
            backend_state: Some("Running".into()),
            dns_name: Some("homelab.example.ts.net".into()),
            tailnet: Some("example.ts.net".into()),
            magic_dns: true,
            cert_domains: vec!["homelab.example.ts.net".into()],
            addresses: vec!["100.101.102.103".into()],
            ..TailscaleReport::default()
        }
    }

    fn tailnet_worker() -> TailnetWorkerFact {
        TailnetWorkerFact {
            id: 2,
            name: "homelab".into(),
            report: tailnet_report(),
            minutes_ago: 1,
            listener_on_tailscale: None,
        }
    }

    fn serve_facts() -> PlanFacts {
        PlanFacts {
            audience: Audience::Private,
            requested: None,
            private_workers: Vec::new(),
            tailnet_workers: vec![tailnet_worker()],
            suggested_private_worker: Some("homelab".into()),
            ..base()
        }
    }

    /// ADR 065 D1: with nothing asked for, a worker whose owner allowed
    /// Serve offers its own name, and nothing goes to DNS or `domains`.
    #[test]
    fn a_private_plan_offers_the_machine_name_of_a_consenting_worker() {
        let facts = serve_facts();
        assert_eq!(
            address_for_audience(&facts),
            Ok(Some("homelab.example.ts.net".into()))
        );
        let plan = plan(&facts);
        assert_eq!(kinds(&plan), vec![PlanOptionKind::TailscaleServe]);
        assert_eq!(plan.address.as_deref(), Some("homelab.example.ts.net"));
        let option = &plan.options[0];
        assert_eq!(option.way_in, "private:homelab");
        assert_eq!(
            option.steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![
                StepKind::AttachDomain,
                StepKind::SetAudience,
                StepKind::ConfigureTailscaleServe,
                StepKind::CheckReachability
            ]
        );
        assert!(option.warnings[0].contains("Another app on `homelab` needs a Service name"));
        assert!(option.warnings[1].contains("public certificate logs"));

        // Asked for by name, the same option.
        let asked = plan_of(&PlanFacts {
            requested: Some("HOMELAB.example.ts.net.".into()),
            ..serve_facts()
        });
        assert_eq!(kinds(&asked), vec![PlanOptionKind::TailscaleServe]);
    }

    /// ADR 065 D1/D5: a name under the tailnet that is no machine's needs
    /// a Service, which only a tagged device can host.
    #[test]
    fn a_service_name_on_an_untagged_worker_is_refused_with_d5s_message() {
        let plan = plan_of(&PlanFacts {
            requested: Some("myapp.example.ts.net".into()),
            ..serve_facts()
        });
        assert!(plan.options.is_empty());
        let blocked = plan.blocked.expect("blocked");
        assert_eq!(
            blocked.reason,
            tailnet_messages::not_tagged("homelab", "a user", "homelab.example.ts.net")
        );
        assert_eq!(
            blocked.action.and_then(|a| a.command).as_deref(),
            Some("sudo tailscale up --advertise-tags=tag:temps")
        );
    }

    fn service_facts() -> PlanFacts {
        let mut worker = tailnet_worker();
        worker.report.tags = vec!["tag:temps".into()];
        PlanFacts {
            requested: Some("myapp.example.ts.net".into()),
            tailnet_workers: vec![worker],
            install_id: "inst1".into(),
            ..serve_facts()
        }
    }

    fn credential(refusal: Option<&str>) -> TailscaleCredentialFact {
        TailscaleCredentialFact {
            id: 31,
            name: "tailscale".into(),
            refusal: refusal.map(str::to_string),
        }
    }

    /// ADR 065 D1/D2/D4: with no credential, the Service way in asks for
    /// one, says the scope is tailnet-wide, and shows the autoApprovers
    /// snippet; it never approves.
    #[test]
    fn a_service_name_is_offered_with_a_pasted_oauth_client() {
        let plan = plan_of(&service_facts());
        assert_eq!(
            kinds(&plan),
            vec![PlanOptionKind::TailscaleServiceWithCredential]
        );
        let option = &plan.options[0];
        assert_eq!(
            option.steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![
                StepKind::SaveTailscaleCredential,
                StepKind::AttachDomain,
                StepKind::SetAudience,
                StepKind::DefineTailscaleService,
                StepKind::ConfigureTailscaleServe,
                StepKind::WaitForServiceApproval,
                StepKind::CheckReachability,
            ]
        );
        assert_eq!(
            option
                .fields
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            TailscaleFactory::fields()
                .iter()
                .map(|f| f.key.clone())
                .collect::<Vec<_>>()
        );
        assert!(option.warnings[0]
            .contains("\"autoApprovers\": {\"services\": {\"svc:myapp\": [\"tag:temps\"]}}"));
        assert!(option.warnings[0].contains("never approves hosts itself"));
        assert!(option.warnings.iter().any(|w| w.contains("tailnet-wide")));
        assert!(option.steps[3].label.contains("Create `svc:myapp`"));
    }

    /// ADR 065 D1: with a usable credential the Service is the first
    /// choice, before the machine name; one whose scopes fail is refused
    /// with D5's message and is never worked around.
    #[test]
    fn a_usable_credential_makes_the_service_the_first_choice() {
        let facts = PlanFacts {
            requested: None,
            tailscale_credential: Some(credential(None)),
            ..service_facts()
        };
        let address = address_for_audience(&facts)
            .expect("an address")
            .expect("suggested");
        assert!(address.ends_with(".example.ts.net"), "{address}");
        assert_ne!(address, "homelab.example.ts.net");
        let plan = plan_of(&PlanFacts {
            requested: Some("myapp.example.ts.net".into()),
            ..facts.clone()
        });
        assert_eq!(kinds(&plan), vec![PlanOptionKind::TailscaleService]);
        assert!(plan.options[0].fields.is_empty());
        assert!(!plan.options[0]
            .steps
            .iter()
            .any(|s| s.kind == StepKind::SaveTailscaleCredential));

        let refused = plan_of(&PlanFacts {
            tailscale_credential: Some(credential(Some(tailnet_messages::CREDENTIAL_SCOPE))),
            ..service_facts()
        });
        assert!(refused.options.is_empty());
        assert_eq!(
            refused.blocked.expect("blocked").reason,
            tailnet_messages::CREDENTIAL_SCOPE
        );
    }

    /// ADR 065 D4: a Service at the name that this install did not make
    /// is never used; this install's own is, only as Temps defines it; an
    /// unreadable name offers nothing.
    #[test]
    fn a_service_temps_did_not_make_is_never_offered() {
        let seen = |definition: ServiceDefinition| PlanFacts {
            service_seen: Some(("svc:myapp".into(), ServiceSeen::Found(definition))),
            tailscale_credential: Some(credential(None)),
            ..service_facts()
        };
        let foreign = plan_of(&seen(ServiceDefinition {
            name: "svc:myapp".into(),
            addrs: vec!["100.100.1.9".into()],
            comment: "the media server".into(),
            ports: vec!["tcp:443".into()],
            tags: vec!["tag:media".into()],
        }));
        assert!(foreign.options.is_empty());
        assert_eq!(
            foreign.blocked.expect("blocked").reason,
            tailnet_messages::service_taken("svc:myapp")
        );

        let other_install = plan_of(&seen(ServiceDefinition::fixed(
            "svc:myapp",
            "tag:temps",
            &tailscale::marker("another-install", 3),
        )));
        assert!(other_install.options.is_empty());

        let ours = plan_of(&seen(ServiceDefinition::fixed(
            "svc:myapp",
            "tag:temps",
            &tailscale::marker("inst1", 3),
        )));
        assert_eq!(kinds(&ours), vec![PlanOptionKind::TailscaleService]);
        assert!(ours.options[0].steps[2]
            .label
            .contains("which this Temps install made"));

        let mut changed =
            ServiceDefinition::fixed("svc:myapp", "tag:temps", &tailscale::marker("inst1", 3));
        changed.ports.push("tcp:8443".into());
        let changed = plan_of(&seen(changed));
        assert!(changed.options.is_empty());
        assert!(changed
            .blocked
            .expect("blocked")
            .reason
            .contains("Temps never changes a Service"));

        let unreadable = plan_of(&PlanFacts {
            service_seen: Some((
                "svc:myapp".into(),
                ServiceSeen::Unreadable("403 Forbidden".into()),
            )),
            ..service_facts()
        });
        assert!(unreadable.options.is_empty());
        assert!(unreadable
            .blocked
            .expect("blocked")
            .reason
            .contains("403 Forbidden"));
    }

    /// ADR 065 D5: each reason the worker cannot serve its name, in the
    /// agent's order, with D5's message.
    #[test]
    fn the_machine_name_is_refused_with_d5s_message_for_each_cause() {
        let refused = |edit: &dyn Fn(&mut TailnetWorkerFact)| {
            let mut worker = tailnet_worker();
            edit(&mut worker);
            let facts = PlanFacts {
                requested: Some("homelab.example.ts.net".into()),
                tailnet_workers: vec![worker],
                ..serve_facts()
            };
            let plan = plan_of(&facts);
            assert!(plan.options.is_empty(), "{:?}", plan.options);
            plan.blocked.expect("blocked")
        };
        let no_consent = refused(&|w| w.report.consent = false);
        assert_eq!(
            no_consent.reason,
            "Run `temps agent ingress private --listen tailscale --tailscale-serve` on `homelab`."
        );
        assert_eq!(
            no_consent.action.and_then(|a| a.command).as_deref(),
            Some(temps_core::tailnet::TAILSCALE_SERVE_COMMAND)
        );
        assert!(refused(&|w| {
            w.report.root = false;
            w.report.user = Some("temps".into());
        })
        .reason
        .contains("runs as `temps`"));
        assert!(
            refused(&|w| w.report.state = temps_core::tailnet::TailscaleState::NoSocket)
                .reason
                .contains("/var/run/tailscale/tailscaled.sock")
        );
        assert!(refused(&|w| {
            w.report.state = temps_core::tailnet::TailscaleState::Unsupported;
            w.report.version = Some("1.90.2".into());
        })
        .reason
        .contains("Temps needs 1.94 or later"));
        assert!(
            refused(&|w| w.report.backend_state = Some("NeedsLogin".into()))
                .reason
                .contains("`NeedsLogin`")
        );
        assert!(refused(&|w| w.report.magic_dns = false)
            .reason
            .starts_with("MagicDNS is off for tailnet `example.ts.net`"));
        assert!(refused(&|w| w.report.cert_domains.clear())
            .reason
            .starts_with("HTTPS certificates are off for tailnet `example.ts.net`"));
        assert!(
            refused(&|w| w.report.port_443_held_by = Some("http://127.0.0.1:3000".into()))
                .reason
                .contains("already serves `http://127.0.0.1:3000`")
        );
        assert!(refused(&|w| w.report.port_443_funnel = true)
            .reason
            .contains("Tailscale Funnel"));
        assert!(refused(&|w| w.minutes_ago = 45)
            .reason
            .contains("has not reported for 45 minutes"));
        // ADR 065 D1: the phase 2 listener on the same Tailscale address.
        assert!(refused(
            &|w| w.listener_on_tailscale = Some("100.101.102.103".parse().expect("ip"))
        )
        .reason
        .contains("already serves private addresses on 100.101.102.103:443"));
    }

    /// ADR 065 D1: the listener and the machine name never share a worker's
    /// Tailscale address, refused whichever comes second.
    #[test]
    fn the_listener_is_refused_where_serve_holds_the_tailscale_address() {
        let facts = PlanFacts {
            serve_entries: vec![ServeEntryFact {
                node_id: 2,
                hostname: "homelab.example.ts.net".into(),
                via: "tailscale_serve".into(),
                service: None,
                removing: false,
            }],
            ..private_facts()
        };
        let plan = plan_of(&facts);
        assert!(plan.options.is_empty());
        let reason = plan.blocked.expect("blocked").reason;
        assert!(
            reason.contains("serves `homelab.example.ts.net` with Tailscale Serve"),
            "{reason}"
        );
        // An entry being removed no longer holds it.
        let mut removing = facts.clone();
        removing.serve_entries[0].removing = true;
        assert_eq!(
            kinds(&plan_of(&removing)),
            vec![PlanOptionKind::PrivateWorker]
        );
        // ...but the machine name waits for it to go.
        let waiting = plan_of(&PlanFacts {
            serve_entries: removing.serve_entries.clone(),
            ..serve_facts()
        });
        assert!(waiting.options.is_empty());
    }

    /// Nothing asked for, no listener and a worker that cannot serve its
    /// name: the plan says why that worker cannot.
    #[test]
    fn with_no_way_in_the_consenting_workers_problem_is_shown() {
        let mut worker = tailnet_worker();
        worker.report.magic_dns = false;
        let plan = plan_of(&PlanFacts {
            tailnet_workers: vec![worker],
            zones: vec![zone("example.com", "cloudflare")],
            ..serve_facts()
        });
        assert!(plan.options.is_empty());
        assert!(plan
            .blocked
            .expect("blocked")
            .reason
            .starts_with("MagicDNS is off"));
    }

    /// The plan is held to what the worker reported (ADR 064 D4).
    #[test]
    fn the_fingerprint_follows_the_workers_tailscale_report() {
        let facts = serve_facts();
        let mut changed = serve_facts();
        changed.tailnet_workers[0].report.port_443_held_by = Some("http://127.0.0.1:3000".into());
        let name = Some("homelab.example.ts.net");
        assert_ne!(fingerprint(&facts, name), fingerprint(&changed, name));
        // Inside checks change on every pass and do not stale a plan.
        let mut checked = serve_facts();
        checked.tailnet_workers[0].report.checks = vec![temps_core::tailnet::TailnetCheck {
            address_id: "custom:4".into(),
            hostname: "homelab.example.ts.net".into(),
            outcome: temps_core::tailnet::TailnetCheckOutcome::Reachable,
            message: String::new(),
        }];
        assert_eq!(fingerprint(&facts, name), fingerprint(&checked, name));
    }

    fn alice() -> Vec<temps_core::tailnet::TailnetPrincipal> {
        vec![temps_core::tailnet::TailnetPrincipal::parse("alice@example.com").expect("a login")]
    }

    fn users_facts() -> PlanFacts {
        PlanFacts {
            audience: Audience::TailnetUsers,
            principals: alice(),
            identity_workers: vec!["homelab".into()],
            ..serve_facts()
        }
    }

    /// ADR 065 D6: an empty list is refused by the plan; nothing is made
    /// reachable to nobody.
    #[test]
    fn a_tailnet_users_plan_needs_someone_listed() {
        let plan = plan_of(&PlanFacts {
            principals: Vec::new(),
            ..users_facts()
        });
        assert!(plan.options.is_empty());
        assert_eq!(plan.audience, Audience::TailnetUsers);
        assert_eq!(
            plan.blocked.expect("blocked").reason,
            tailnet_messages::NO_PRINCIPALS
        );
    }

    #[test]
    fn a_tailnet_users_plan_serves_the_machine_name_and_says_who_is_admitted() {
        let plan = plan_of(&users_facts());
        assert_eq!(kinds(&plan), vec![PlanOptionKind::TailscaleServe]);
        assert_eq!(plan.audience, Audience::TailnetUsers);
        assert_eq!(plan.principals, alice());
        let option = &plan.options[0];
        assert!(option.recommended);
        assert!(
            option.warnings[0]
                .starts_with("Only alice@example.com can open homelab.example.ts.net"),
            "{:?}",
            option.warnings
        );
        let audience = option
            .steps
            .iter()
            .find(|s| s.kind == StepKind::SetAudience)
            .expect("an audience step");
        assert!(
            audience.label.contains("only for alice@example.com"),
            "{}",
            audience.label
        );
    }

    /// D6: an agent that cannot check who a visitor is never gets the
    /// address; the plan says which agent to update.
    #[test]
    fn a_tailnet_users_plan_refuses_a_worker_whose_agent_cannot_check_identity() {
        let plan = plan_of(&PlanFacts {
            identity_workers: Vec::new(),
            ..users_facts()
        });
        assert!(plan.options.is_empty());
        let reason = plan.blocked.expect("blocked").reason;
        assert!(reason.contains("Update the agent on `homelab`"), "{reason}");
    }

    /// D6: on the private listener whois needs a Tailscale listener and
    /// the owner's consent; without either the plan says what to do.
    #[test]
    fn a_tailnet_users_listener_needs_tailscale_and_the_owners_consent() {
        let listener = || PlanFacts {
            audience: Audience::TailnetUsers,
            principals: alice(),
            identity_workers: vec!["homelab".into()],
            ..private_facts()
        };
        let plan = plan_of(&listener());
        assert!(plan.options.is_empty());
        let blocked = plan.blocked.expect("blocked");
        assert_eq!(
            blocked.reason,
            tailnet_messages::users_need_consent("homelab")
        );
        let action = blocked.action.expect("action");
        assert_eq!(action.kind, ActionKind::RunCommand);
        assert_eq!(
            action.command.as_deref(),
            Some(temps_core::tailnet::TAILSCALE_SERVE_COMMAND)
        );

        let mut off_tailnet = private_worker_fact(None);
        off_tailnet.tailscale = false;
        off_tailnet.listener = Some("192.168.1.20".parse().expect("ip"));
        let plan = plan_of(&PlanFacts {
            private_workers: vec![off_tailnet],
            ..listener()
        });
        assert!(plan.options.is_empty());
        assert_eq!(
            plan.blocked.expect("blocked").reason,
            tailnet_messages::users_need_tailnet("192.168.1.20")
        );
    }

    // ----- ADR 069 D3: what is possible before an address is asked for ---

    /// The user's example: production runs on the control plane. A public
    /// address is still possible through a direct ingress, and the choice
    /// says up front why no tunnel is offered.
    #[test]
    fn a_choice_says_why_no_tunnel_is_offered_on_the_control_plane() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            directs: vec![direct(true)],
            served_by_workers: false,
            ..base()
        };
        let choice = choice(&facts);
        assert!(choice.available);
        assert!(choice.blocked.is_none());
        assert!(!choice.ways_in.is_empty());
        assert!(choice
            .ways_in
            .iter()
            .all(|w| !w.way_in.starts_with("tunnel:")));
        assert!(choice.ways_in[0].recommended);
        assert_eq!(choice.not_offered.len(), 1);
        assert!(
            choice.not_offered[0]
                .reason
                .contains("runs on the control plane"),
            "{:?}",
            choice.not_offered
        );
    }

    /// A private-only allow list keeps tunnels out while direct ingress still
    /// fits: the choice is offered and says why no tunnel is.
    #[test]
    fn a_choice_says_why_a_private_allow_list_leaves_out_the_tunnel() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            tunnel_workers: vec![worker()],
            directs: vec![direct(true)],
            zones: vec![zone("example.com", "cloudflare")],
            private_rule: Some(PrivateAllowList {
                rule: "the IP access allow list of environment `production`".into(),
                cidr: "100.64.0.0/10".into(),
            }),
            ..base()
        };
        let choice = choice(&facts);
        assert!(choice.available);
        assert!(choice
            .ways_in
            .iter()
            .all(|w| !w.way_in.starts_with("tunnel:")));
        assert_eq!(choice.not_offered.len(), 1, "{:?}", choice.not_offered);
        let reason = &choice.not_offered[0].reason;
        assert!(
            reason.contains("100.64.0.0/10") && reason.contains("environment `production`"),
            "{reason}"
        );
        // Without the rule a tunnel is offered and nothing is left out.
        let choice = super::choice(&PlanFacts {
            private_rule: None,
            ..facts
        });
        assert!(choice
            .ways_in
            .iter()
            .any(|w| w.way_in.starts_with("tunnel:")));
        assert!(choice.not_offered.is_empty());
    }

    /// With nothing but a tunnel, the same environment cannot be public at
    /// all, and the reason is the planner's.
    #[test]
    fn a_choice_is_unavailable_with_the_planners_reason() {
        let facts = PlanFacts {
            tunnels: vec![tunnel("homelab", "example.com", "example.com")],
            served_by_workers: false,
            ..base()
        };
        let choice = choice(&facts);
        assert!(!choice.available);
        assert!(choice.ways_in.is_empty());
        assert!(choice.not_offered.is_empty());
        let blocked = choice.blocked.expect("blocked");
        assert!(
            blocked.reason.contains("runs on the control plane"),
            "{}",
            blocked.reason
        );
        // The plan the user would get says the same.
        assert_eq!(
            plan(&facts).blocked.expect("blocked").reason,
            blocked.reason
        );
    }

    /// A choice is for the environment, not for whatever address the facts
    /// were gathered with.
    #[test]
    fn a_choice_ignores_the_address_asked_for() {
        let facts = PlanFacts {
            requested: Some("localhost".into()),
            directs: vec![direct(true)],
            ..base()
        };
        assert!(plan(&facts).options.is_empty());
        assert!(choice(&facts).available);
    }

    /// ADR 065 D6: before anyone is listed, `tailnet_users` is available
    /// when a way in can tell who a visitor is, and the plan to apply still
    /// refuses an empty list.
    #[test]
    fn a_tailnet_users_choice_is_judged_before_anyone_is_listed() {
        let unlisted = PlanFacts {
            principals: Vec::new(),
            ..users_facts()
        };
        let choice = choice(&unlisted);
        assert!(choice.available, "{:?}", choice.blocked);
        assert_eq!(choice.audience, Audience::TailnetUsers);
        assert_eq!(
            choice.ways_in.iter().map(|w| w.kind).collect::<Vec<_>>(),
            vec![PlanOptionKind::TailscaleServe]
        );
        assert!(plan_of(&unlisted).options.is_empty());

        // An agent that cannot check identities leaves nothing to offer.
        let unchecked = PlanFacts {
            identity_workers: Vec::new(),
            ..unlisted
        };
        let choice = super::choice(&unchecked);
        assert!(!choice.available);
        assert!(choice.blocked.is_some());
    }

    /// Only `signed_in` names a gate.
    #[test]
    fn a_choice_names_the_gate_only_for_signed_in() {
        let public = choice(&PlanFacts {
            directs: vec![direct(true)],
            ..base()
        });
        assert_eq!(public.gate, None);
        let signed_in = choice(&PlanFacts {
            audience: Audience::SignedIn,
            gate: GateKind::Temps,
            directs: vec![direct(true)],
            ..base()
        });
        assert_eq!(signed_in.gate, Some(GateKind::Temps));
    }
}
