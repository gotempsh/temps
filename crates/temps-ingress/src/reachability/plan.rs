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
use temps_core::ways_in::{private_allow_list_message, PrivateAllowList};

use temps_auth::permissions::Permission;

use super::compute::{is_local_only, DirectIngress, INGRESS_SETUP_PATH, PRIVATE_COMMAND};
use super::model::{
    ActionKind, AddressAudience, Audience, PlanBlocked, PlanOption, PlanOptionKind, PlanStep,
    PlannedRecord, ReachAction, ReachabilityPlan, StepKind,
};
use crate::registry::{FieldKind, SetupField};
use crate::service::{covers, normalize_host, record_label, PublicationIntent, TunnelFact};

/// Field keys of the apply request.
pub const FIELD_ADDRESS: &str = "address";
pub const FIELD_DNS_PROVIDER: &str = "dns_provider_id";
pub const FIELD_TOKEN: &str = "cloudflare_api_token";

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
    if facts.audience == Audience::Private {
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
    let canonical = format!(
        "v3|{:?}|{address:?}|{audiences:?}|{}|{}|{}|{tunnels:?}|{choices:?}|{zones:?}|\
         {accounts:?}|{directs:?}|{modes:?}|{intent:?}|{:?}|{records:?}|{private_workers:?}|\
         {ranges:?}",
        facts.audience,
        facts.deployed,
        facts.served_by_workers,
        address.is_some_and(|a| facts.taken_elsewhere.contains(a)),
        facts.private_rule,
    );
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
    }
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
             address's zone only. Temps checks it before creating anything and stores it \
             encrypted.",
            cloudflare_token_url()
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

/// The plan for `facts`.
pub fn plan(facts: &PlanFacts) -> ReachabilityPlan {
    let mut blocked_reasons: Vec<PlanBlocked> = Vec::new();
    let empty = |audience, address, address_error, blocked| ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience,
        address,
        address_error,
        options: Vec::new(),
        blocked: Some(blocked),
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
    if facts.audience != Audience::Public {
        return empty(
            facts.audience,
            facts.requested.clone(),
            None,
            PlanBlocked {
                reason: "Addresses for people who sign in are not available yet. Choose \
                         public, or only your private network."
                    .to_string(),
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
        .and_then(|host| becomes_public(facts, host));

    // A tunnel only carries what workers serve, and never into an allow
    // list its visitors cannot match (D9).
    let tunnels_served = !facts.deployed || facts.served_by_workers;
    let tunnels_possible = tunnels_served && facts.private_rule.is_none();
    let mut d9_note: Option<String> = None;
    if let (Some(rule), true) = (&facts.private_rule, tunnels_served) {
        let through = address
            .as_deref()
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
        let reason = format!(
            "No tunnel is offered: {} The rule is {}.",
            private_allow_list_message(&through, &rule.cidr, &shown),
            rule.rule
        );
        d9_note = Some(reason.clone());
        blocked_reasons.push(PlanBlocked {
            reason,
            action: None,
        });
    }
    if !tunnels_served {
        blocked_reasons.push(PlanBlocked {
            reason: format!(
                "{} runs on the control plane, and a tunnel only carries what workers serve. \
                 Deploy it to a worker to put it behind a tunnel.",
                facts.environment_name
            ),
            action: None,
        });
    }
    let not_deployed_warning = (!facts.deployed).then(|| {
        format!(
            "Nothing is deployed to {} yet: it is published once a deployment runs on a \
             worker.",
            facts.environment_name
        )
    });
    let mut drafts: Vec<OptionDraft> = Vec::new();

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
        address,
        address_error: None,
        options,
        blocked,
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

/// When `host` is attached here as a private address: what making it
/// public exposes, in words (ADR 064 Security).
pub fn becomes_public(facts: &PlanFacts, host: &str) -> Option<String> {
    let (_, audience) = facts.audiences.iter().find(|(a, _)| a == host)?;
    if *audience == Audience::Public {
        return None;
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

/// The plan for a private address (ADR 064 D5): one worker's private
/// listener, DNS in a zone Temps manages, a DNS-01 certificate.
fn plan_private(facts: &PlanFacts) -> ReachabilityPlan {
    let blocked_plan = |address: Option<String>, address_error, blocked| ReachabilityPlan {
        plan_id: None,
        fingerprint: None,
        expires_at: None,
        audience: Audience::Private,
        address,
        address_error,
        options: Vec::new(),
        blocked: Some(blocked),
    };
    let block = |reason: String, action: Option<ReachAction>| PlanBlocked { reason, action };
    let ask_address = facts
        .requested
        .as_deref()
        .is_none_or(|r| r.trim().is_empty());
    let address = match facts.requested.as_deref().map(str::trim) {
        Some(r) if !r.is_empty() => match planned_address(facts) {
            Ok(address) => address,
            Err(error) => {
                return blocked_plan(
                    facts.requested.clone(),
                    Some(error.clone()),
                    block(error, None),
                )
            }
        },
        // A private address is suggested only under a zone Temps manages:
        // it needs a DNS record and a DNS-01 certificate there.
        _ => {
            let label = suggested_label(facts);
            facts
                .zones
                .iter()
                .filter(|z| z.verified)
                .map(|z| format!("{label}.{}", z.domain.trim_end_matches('.')))
                .find(|c| !facts.taken_elsewhere.contains(c) && !facts.attached_here.contains(c))
        }
    };
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
        address,
        address_error: None,
        options: vec![option],
        blocked: None,
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
                audiences: vec![("app.example.com".into(), Audience::parse("tailnet_users"))],
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
        // The address was given, so it is not asked again.
        assert!(plan.options[0].fields.is_empty());
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
        assert_eq!(option.fields.len(), 1);
        assert_eq!(option.fields[0].key, FIELD_TOKEN);
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
}
