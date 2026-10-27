// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tunnel environment hosts (ADR 068 D3): with `environment_hosts` on, one
//! tunnel publishes every environment as `<label>.<connector.domain>`.
//!
//! The rows are `environment_domains` rows holding the full hostname, marked
//! with `generated_by_connector_id`. This module decides which rows should
//! exist ([`plan_rows`], pure) and keeps them so ([`EnvironmentHostService`]).
//! Publication itself is the ADR 058 path: the connector's plan treats a
//! generated row, and its Flat service hosts, as chosen for that tunnel.
//!
//! Rules this module keeps:
//!
//! - It never touches a row without `generated_by_connector_id`.
//! - A name taken under the zone by a DNS record Temps does not own (ADR 031),
//!   by another address in this install (a custom domain, an environment
//!   domain, a status page, the console), or by another environment's
//!   generated host, gets the short-hash name `<label>-<hash>.<zone>`
//!   instead, and the listing says why. One hostname is never generated for
//!   two environments.
//! - A rename publishes the new host before the old one is withdrawn: the old
//!   row stays until the new host is published.
//! - A deleted environment, or one whose addresses are `private` or
//!   `signed_in`, loses its generated rows.
//! - Once the preview domain is usable, existing hosts stay and no new ones
//!   are generated.

use std::collections::{HashMap, HashSet};

use temps_core::public_hostname::{environment_label, hashed_label, single_dns_label};

/// What [`plan_rows`] needs to know about one environment.
#[derive(Debug, Clone)]
pub struct EnvironmentFacts {
    pub environment_id: i32,
    pub project_slug: String,
    pub environment_name: String,
    /// The environment's hostname label (`environments.subdomain`).
    pub label: String,
    pub is_production: bool,
    /// Gone, hosted elsewhere, or restricted to a non-public audience
    /// (`private`, `signed_in`, `tailnet_users`): no generated host.
    pub excluded: Option<Exclusion>,
}

/// Why an environment gets no tunnel host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    Deleted,
    HostedElsewhere,
    /// An address of it is not public: `private` (a listener, a tailnet name
    /// through Serve or a Tailscale Service), `signed_in` or `tailnet_users`
    /// (ADR 064, ADR 065). A public tunnel host would widen who can reach
    /// it, so it is refused.
    Restricted,
}

impl Exclusion {
    pub fn reason(self) -> &'static str {
        match self {
            Exclusion::Deleted => "the environment was deleted",
            Exclusion::HostedElsewhere => "the project is hosted elsewhere",
            Exclusion::Restricted => {
                "its addresses are private, for signed-in people or for listed tailnet users; a \
                 public tunnel host would widen who can reach it"
            }
        }
    }
}

/// The environments any of whose own addresses (`environment_domains`
/// rows Temps did not generate, and custom domains) is not public.
fn restricted_environments<'a>(
    own: impl Iterator<Item = (i32, &'a str, bool)>,
    custom: impl Iterator<Item = (i32, &'a str)>,
) -> HashSet<i32> {
    own.filter(|(_, audience, generated)| !generated && *audience != PUBLIC)
        .map(|(environment_id, _, _)| environment_id)
        .chain(
            custom
                .filter(|(_, audience)| *audience != PUBLIC)
                .map(|(environment_id, _)| environment_id),
        )
        .collect()
}

/// A generated `environment_domains` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedRow {
    pub id: i32,
    pub environment_id: i32,
    pub domain: String,
}

/// The hostname label an environment publishes under: production follows
/// the operator's `production_hostname_template` when one is set; every
/// other environment, and production without a template, uses its own label.
/// Always one ASCII DNS label of at most 63 characters: a preview label
/// built from a long or non-ASCII branch name, or an old subdomain with
/// dots, is sanitised and capped with a short hash.
pub fn host_label(env: &EnvironmentFacts, production_template: Option<&str>) -> String {
    match production_template.map(str::trim).filter(|t| !t.is_empty()) {
        Some(template) if env.is_production => environment_label(
            None,
            Some(template),
            &env.project_slug,
            &env.environment_name,
        ),
        _ => single_dns_label(&env.label),
    }
}

/// [`host_label`] for an environment row: what the D5 offer and the plan
/// show before any row exists.
pub fn environment_host_label(
    project_slug: &str,
    environment: &temps_entities::environments::Model,
    production_template: Option<&str>,
) -> String {
    let facts = EnvironmentFacts {
        environment_id: environment.id,
        project_slug: project_slug.to_string(),
        environment_name: environment.name.clone(),
        label: environment.subdomain.clone(),
        is_production: environment.name.eq_ignore_ascii_case("production"),
        excluded: None,
    };
    host_label(&facts, production_template)
}

/// `<label>.<domain>`.
pub fn natural_host(label: &str, domain: &str) -> String {
    format!(
        "{}.{}",
        label.to_ascii_lowercase(),
        domain.trim_end_matches('.').to_ascii_lowercase()
    )
}

/// `<label>-<hash>.<domain>`: the fallback when the natural name is taken.
/// The hash is of the environment and the natural name, so the fallback is
/// stable across passes.
pub fn fallback_host(label: &str, domain: &str, environment_id: i32) -> String {
    let seed = format!("{}#{environment_id}", natural_host(label, domain));
    natural_host(&hashed_label(label, &seed), domain)
}

/// Whether `domain` is one of the names this module could have generated for
/// `label`: what a pass keeps without asking DNS again.
pub fn is_generated_name(domain: &str, label: &str, zone: &str, environment_id: i32) -> bool {
    let domain = domain.to_ascii_lowercase();
    domain == natural_host(label, zone) || domain == fallback_host(label, zone, environment_id)
}

/// The rows to insert and delete in one pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RowChanges {
    /// `(environment_id, hostname)`.
    pub insert: Vec<(i32, String)>,
    /// Row ids; only ever generated rows.
    pub delete: Vec<i32>,
}

/// Decide the rows for one pass.
///
/// `desired` is each eligible environment's host, already resolved to the
/// fallback when the natural name is taken. `published` holds the hosts the
/// tunnel has published, which is what lets a renamed environment's old row
/// go. `generate_new` is false once the preview domain is usable.
pub fn plan_rows(
    envs: &[EnvironmentFacts],
    rows: &[GeneratedRow],
    desired: &HashMap<i32, String>,
    published: &HashSet<String>,
    generate_new: bool,
) -> RowChanges {
    let mut changes = RowChanges::default();
    let by_id: HashMap<i32, &EnvironmentFacts> =
        envs.iter().map(|e| (e.environment_id, e)).collect();
    let mut rows_of: HashMap<i32, Vec<&GeneratedRow>> = HashMap::new();
    for row in rows {
        rows_of.entry(row.environment_id).or_default().push(row);
    }
    // A row holding the name another environment is given goes at once:
    // one hostname is never served for two environments.
    let wanted_by: HashMap<String, i32> = desired
        .iter()
        .map(|(environment_id, host)| (host.to_ascii_lowercase(), *environment_id))
        .collect();
    changes.delete.extend(
        rows.iter()
            .filter(|row| {
                wanted_by
                    .get(&row.domain.to_ascii_lowercase())
                    .is_some_and(|holder| *holder != row.environment_id)
            })
            .map(|row| row.id),
    );

    for (environment_id, env_rows) in &rows_of {
        let eligible = by_id
            .get(environment_id)
            .is_some_and(|env| env.excluded.is_none());
        let wanted = desired.get(environment_id).filter(|_| eligible);
        let Some(wanted) = wanted else {
            // Gone, excluded, or no longer wanted: withdraw everything.
            changes.delete.extend(env_rows.iter().map(|r| r.id));
            continue;
        };
        let has_wanted = env_rows
            .iter()
            .any(|r| r.domain.eq_ignore_ascii_case(wanted));
        if !has_wanted {
            // Renamed: publish the new name first.
            changes.insert.push((*environment_id, wanted.clone()));
        }
        // The old names go once the new one is published.
        let new_is_live = published.contains(&wanted.to_ascii_lowercase());
        for row in env_rows {
            if !row.domain.eq_ignore_ascii_case(wanted) && new_is_live {
                changes.delete.push(row.id);
            }
        }
        // Two rows with the wanted name: keep one.
        let mut seen = false;
        for row in env_rows {
            if row.domain.eq_ignore_ascii_case(wanted) {
                if seen {
                    changes.delete.push(row.id);
                }
                seen = true;
            }
        }
    }

    if generate_new {
        for env in envs {
            if env.excluded.is_some() || rows_of.contains_key(&env.environment_id) {
                continue;
            }
            if let Some(wanted) = desired.get(&env.environment_id) {
                changes.insert.push((env.environment_id, wanted.clone()));
            }
        }
    }
    changes.insert.sort();
    changes.delete.sort_unstable();
    changes.delete.dedup();
    changes
}

/// Which environment holds each generated name: the oldest row wins when
/// two hold one name.
pub fn holders(rows: &[GeneratedRow]) -> HashMap<String, i32> {
    let mut sorted: Vec<&GeneratedRow> = rows.iter().collect();
    sorted.sort_by_key(|r| r.id);
    let mut holder = HashMap::new();
    for row in sorted {
        holder
            .entry(row.domain.to_ascii_lowercase())
            .or_insert(row.environment_id);
    }
    holder
}

/// The generated row an environment keeps: one of its names for its
/// current label that no other environment holds.
fn kept_row<'a>(
    env: &EnvironmentFacts,
    label: &str,
    domain: &str,
    rows: &'a [GeneratedRow],
    holder: &HashMap<String, i32>,
) -> Option<&'a GeneratedRow> {
    rows.iter().find(|r| {
        r.environment_id == env.environment_id
            && is_generated_name(&r.domain, label, domain, env.environment_id)
            && holder.get(&r.domain.to_ascii_lowercase()) == Some(&env.environment_id)
    })
}

/// The environments that get a new name this pass, in order, with their
/// labels: every eligible one without a row it keeps, at most
/// [`MAX_NEW_HOSTS_PER_PASS`] of those that have no row at all. One that has
/// rows (a rename) is never put off, or its current host would be
/// withdrawn.
fn needing_names<'a>(
    envs: &'a [EnvironmentFacts],
    rows: &[GeneratedRow],
    production_template: Option<&str>,
    domain: &str,
) -> Vec<(&'a EnvironmentFacts, String)> {
    let holder = holders(rows);
    let mut fresh = 0usize;
    let mut out = Vec::new();
    for env in envs.iter().filter(|e| e.excluded.is_none()) {
        let label = host_label(env, production_template);
        if kept_row(env, &label, domain, rows, &holder).is_some() {
            continue;
        }
        if !rows.iter().any(|r| r.environment_id == env.environment_id) {
            if fresh >= MAX_NEW_HOSTS_PER_PASS {
                continue;
            }
            fresh += 1;
        }
        out.push((env, label));
    }
    out
}

/// The natural names whose DNS a pass reads.
fn names_to_check(
    envs: &[EnvironmentFacts],
    rows: &[GeneratedRow],
    production_template: Option<&str>,
    domain: &str,
) -> Vec<String> {
    needing_names(envs, rows, production_template, domain)
        .into_iter()
        .map(|(_, label)| natural_host(&label, domain))
        .collect()
}

/// Each eligible environment's host: the generated name it holds when it
/// still fits its label, else the natural name, else the short-hash
/// fallback. `taken` says a name is held outside this tunnel's generated
/// rows (another address, Temps itself, a foreign DNS record).
///
/// Every name a generated row holds starts out claimed by its environment,
/// so a name another environment holds -- a duplicate label, or a renamed
/// environment's old name kept until the new one is published -- is taken
/// whatever order the environments come in. `publishable` refuses a host
/// the tunnel's zone cannot carry.
pub fn assign_hosts(
    envs: &[EnvironmentFacts],
    rows: &[GeneratedRow],
    production_template: Option<&str>,
    domain: &str,
    taken: &dyn Fn(&str) -> bool,
    publishable: &dyn Fn(&str) -> bool,
) -> HashMap<i32, String> {
    let mut claimed = holders(rows);
    let mut desired = HashMap::new();
    for env in envs.iter().filter(|e| e.excluded.is_none()) {
        let label = host_label(env, production_template);
        if let Some(row) = kept_row(env, &label, domain, rows, &claimed) {
            desired.insert(env.environment_id, row.domain.to_ascii_lowercase());
        }
    }
    for (env, label) in needing_names(envs, rows, production_template, domain) {
        let id = env.environment_id;
        let held = |claimed: &HashMap<String, i32>, host: &str| {
            taken(host) || claimed.get(host).is_some_and(|holder| *holder != id)
        };
        let natural = natural_host(&label, domain);
        let host = if held(&claimed, &natural) {
            fallback_host(&label, domain, id)
        } else {
            natural
        };
        if held(&claimed, &host) {
            tracing::warn!(environment_id = id, %host, "both names for this environment are taken");
            continue;
        }
        if !publishable(&host) {
            tracing::warn!(environment_id = id, %host, "not a hostname the tunnel can publish");
            continue;
        }
        claimed.insert(host.clone(), id);
        desired.insert(id, host);
    }
    desired
}

// ----- the service ----------------------------------------------------

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_core::environment_address::PreviewDomainStatus;
use temps_dns::{DnsRecordType, RecordOwnership};
use temps_entities::{
    environment_domains, environments, ingress_connectors, ingress_publications,
    project_custom_domains, projects,
};
use tracing::{debug, info, warn};
use utoipa::ToSchema;

use super::environment_address::EnvironmentAddressService;
use super::service::{ReachabilityError, ReachabilityService};
use crate::service::{record_label, EnvironmentHostSync};

/// New hosts checked against DNS in one pass. The rest wait for the next
/// pass (a minute later), so a large install never stalls the reconciler.
const MAX_NEW_HOSTS_PER_PASS: usize = 50;
const CONNECTOR_READY: &str = "ready";
const PUBLISHED: &str = "published";
const PUBLIC: &str = "public";

/// Shown before the opt-in, and with the list (ADR 068 Security).
pub const PUBLIC_NAMES_WARNING: &str = "Every environment, including preview environments named \
     after branches, gets a public DNS name under this tunnel's domain. Branch names become \
     public, and each name's certificate appears in Certificate Transparency logs.";

/// One environment's tunnel host, as the opt-in lists it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentHostView {
    pub environment_id: i32,
    pub project_slug: String,
    pub environment_name: String,
    /// The host, or the one it would get.
    #[schema(example = "shop-production.tunnel.example.com")]
    pub hostname: String,
    /// `published`, `pending`, `error`, or `planned` before the opt-in.
    pub state: String,
    pub last_error: Option<String>,
    /// Why the host is not `<label>.<domain>`, when it is the short-hash
    /// name.
    pub note: Option<String>,
}

/// An environment the tunnel does not publish, and why.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentHostRefusal {
    pub environment_id: i32,
    pub project_slug: String,
    pub environment_name: String,
    pub reason: String,
}

/// `GET`/`PUT /ingress/connectors/{id}/environment-hosts`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentHostsResponse {
    pub connector_id: i32,
    pub connector_name: String,
    pub domain: String,
    pub enabled: bool,
    /// The hosts it publishes, or would publish once turned on.
    pub hosts: Vec<EnvironmentHostView>,
    pub refused: Vec<EnvironmentHostRefusal>,
    /// The tunnel that has environment hosts on instead, if another does:
    /// at most one may.
    pub enabled_elsewhere: Option<String>,
    /// The preview domain is usable: existing hosts stay, no new ones are
    /// generated.
    pub preview_domain_usable: bool,
    pub warning: String,
    /// Send back as `confirm` to turn it on or off. It names exactly the
    /// hosts listed here, so a list that changed since it was shown is not
    /// accepted.
    #[schema(example = "3f2a9c1e0b7d4a65")]
    pub confirm_token: String,
}

/// `PUT /ingress/connectors/{id}/environment-hosts`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetEnvironmentHostsRequest {
    pub enabled: bool,
    /// The `confirm_token` of the list that was shown and accepted: turning
    /// on publishes every host in it, turning off withdraws them.
    #[serde(default)]
    pub confirm: Option<String>,
}

/// What a confirmation is bound to: the tunnel, whether it is on, and every
/// listed environment's host.
pub fn confirm_token(connector_id: i32, enabled: bool, hosts: &[EnvironmentHostView]) -> String {
    use sha2::{Digest, Sha256};
    let mut listed: Vec<String> = hosts
        .iter()
        .map(|h| format!("{}={}", h.environment_id, h.hostname.to_ascii_lowercase()))
        .collect();
    listed.sort();
    let mut hasher = Sha256::new();
    hasher.update(format!("{connector_id}|{enabled}|{}", listed.join(",")));
    hex::encode(hasher.finalize())[..16].to_string()
}

/// What one pass read.
struct Snapshot {
    connector: ingress_connectors::Model,
    envs: Vec<EnvironmentFacts>,
    rows: Vec<GeneratedRow>,
    /// Every address in use in the install, lowercased, except this
    /// connector's generated rows: a name in it is taken.
    taken: HashSet<String>,
    /// Which environment holds each of this connector's generated names:
    /// the oldest row wins when two hold one name.
    holder: HashMap<String, i32>,
    settings: temps_core::AppSettings,
}

impl Snapshot {
    /// Held by another address in the install, or by Temps itself (the
    /// console, the public ingress, the preview apex).
    fn is_taken(&self, host: &str) -> bool {
        self.taken.contains(host) || self.settings.is_reserved_hostname(host)
    }

    fn production_template(&self) -> Option<&str> {
        self.settings.production_hostname_template.as_deref()
    }
}

pub struct EnvironmentHostService {
    reachability: Arc<ReachabilityService>,
    addresses: Arc<EnvironmentAddressService>,
}

impl EnvironmentHostService {
    pub fn new(
        reachability: Arc<ReachabilityService>,
        addresses: Arc<EnvironmentAddressService>,
    ) -> Self {
        Self {
            reachability,
            addresses,
        }
    }

    fn db(&self) -> &sea_orm::DatabaseConnection {
        self.reachability.deps.db.as_ref()
    }

    async fn connector(
        &self,
        connector_id: i32,
    ) -> Result<ingress_connectors::Model, ReachabilityError> {
        ingress_connectors::Entity::find_by_id(connector_id)
            .one(self.db())
            .await?
            .filter(|c| c.status != "deleting")
            .ok_or_else(|| {
                ReachabilityError::NotFound(format!("Tunnel {connector_id} does not exist"))
            })
    }

    async fn preview_usable(&self) -> bool {
        match self.addresses.preview_domain_status().await {
            Ok(status) => status.status == PreviewDomainStatus::Usable,
            Err(error) => {
                // Unknown: keep generating; a usable domain only ever stops
                // new hosts, it never withdraws one.
                warn!(%error, "could not tell whether the preview domain is usable");
                false
            }
        }
    }

    async fn snapshot(
        &self,
        connector: ingress_connectors::Model,
    ) -> Result<Snapshot, ReachabilityError> {
        let db = self.db();
        let all_rows = environment_domains::Entity::find().all(db).await?;
        let mut rows: Vec<GeneratedRow> = all_rows
            .iter()
            .filter(|r| r.generated_by_connector_id == Some(connector.id))
            .map(|r| GeneratedRow {
                id: r.id,
                environment_id: r.environment_id,
                domain: r.domain.to_ascii_lowercase(),
            })
            .collect();
        rows.sort_by_key(|r| r.id);
        let holder = holders(&rows);
        let custom = project_custom_domains::Entity::find().all(db).await?;
        let restricted = restricted_environments(
            all_rows.iter().map(|r| {
                (
                    r.environment_id,
                    r.audience.as_str(),
                    r.generated_by_connector_id.is_some(),
                )
            }),
            custom
                .iter()
                .map(|d| (d.environment_id, d.audience.as_str())),
        );
        let mut taken: HashSet<String> = all_rows
            .iter()
            .filter(|r| r.generated_by_connector_id != Some(connector.id))
            .map(|r| r.domain.trim_end_matches('.').to_ascii_lowercase())
            .collect();
        taken.extend(
            custom
                .iter()
                .map(|d| d.domain.trim_end_matches('.').to_ascii_lowercase()),
        );
        taken.extend(
            temps_entities::status_page_domains::Entity::find()
                .filter(temps_entities::status_page_domains::Column::StatusPageId.is_not_null())
                .all(db)
                .await?
                .into_iter()
                .map(|d| d.hostname.trim_end_matches('.').to_ascii_lowercase()),
        );

        let with_rows: HashSet<i32> = rows.iter().map(|r| r.environment_id).collect();
        let environments = environments::Entity::find()
            .filter(
                sea_orm::Condition::any()
                    .add(environments::Column::DeletedAt.is_null())
                    .add(environments::Column::Id.is_in(with_rows.iter().copied())),
            )
            .all(db)
            .await?;
        let project_ids: HashSet<i32> = environments.iter().map(|e| e.project_id).collect();
        let projects: HashMap<i32, projects::Model> = projects::Entity::find()
            .filter(projects::Column::Id.is_in(project_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|p| (p.id, p))
            .collect();
        let mut environments = environments;
        environments.sort_by_key(|e| e.id);
        let envs = environments
            .into_iter()
            .filter_map(|env| {
                let project = projects.get(&env.project_id)?;
                let excluded = if env.deleted_at.is_some() || project.deleted_at.is_some() {
                    Some(Exclusion::Deleted)
                } else if project.hosting == temps_entities::types::Hosting::External {
                    Some(Exclusion::HostedElsewhere)
                } else if restricted.contains(&env.id) {
                    Some(Exclusion::Restricted)
                } else {
                    None
                };
                Some(EnvironmentFacts {
                    environment_id: env.id,
                    project_slug: project.slug.clone(),
                    is_production: env.name.eq_ignore_ascii_case("production"),
                    environment_name: env.name,
                    label: env.subdomain,
                    excluded,
                })
            })
            .collect();
        let settings = self.reachability.settings().await?;
        Ok(Snapshot {
            connector,
            envs,
            rows,
            taken,
            holder,
            settings,
        })
    }

    /// The zone the tunnel's records live in, when it can be told.
    async fn zone(&self, connector: &ingress_connectors::Model) -> Option<String> {
        match self.reachability.deps.connectors.zone_of(connector).await {
            Ok(zone) => Some(zone),
            Err(error) => {
                debug!(%error, connector_id = connector.id, "tunnel zone unknown");
                None
            }
        }
    }

    /// Whether `host` is held under the zone by an A, AAAA or CNAME record
    /// Temps does not own.
    async fn foreign_record(&self, zone: Option<&str>, domain: &str, host: &str) -> bool {
        let Some(zone) = zone else {
            return false;
        };
        let Ok(label) = record_label(host, domain, zone) else {
            return false;
        };
        for record_type in [DnsRecordType::CNAME, DnsRecordType::A, DnsRecordType::AAAA] {
            match self
                .reachability
                .deps
                .records
                .record_ownership(host, &label, record_type)
                .await
            {
                // Free, or this install's own (another address in this
                // install is caught by `taken`).
                Ok(
                    RecordOwnership::NotFound
                    | RecordOwnership::Owned(..)
                    | RecordOwnership::Orphaned(..),
                ) => {}
                Ok(_) => return true,
                Err(error) => {
                    // Cannot tell: the name is tried, and publication
                    // reports a conflict if there is one.
                    debug!(%error, %host, ?record_type, "could not read DNS ownership");
                }
            }
        }
        false
    }

    /// Each eligible environment's host (see [`assign_hosts`]). DNS is read
    /// here, for the natural names [`assign_hosts`] would try.
    async fn desired(&self, snap: &Snapshot) -> HashMap<i32, String> {
        let domain = snap.connector.domain.as_str();
        let zone = self.zone(&snap.connector).await;
        let mut foreign = HashSet::new();
        for natural in names_to_check(&snap.envs, &snap.rows, snap.production_template(), domain) {
            if !snap.is_taken(&natural)
                && self.foreign_record(zone.as_deref(), domain, &natural).await
            {
                foreign.insert(natural);
            }
        }
        assign_hosts(
            &snap.envs,
            &snap.rows,
            snap.production_template(),
            domain,
            &|host| snap.is_taken(host) || foreign.contains(host),
            &|host| {
                zone.as_deref()
                    .is_none_or(|zone| record_label(host, domain, zone).is_ok())
            },
        )
    }

    async fn published(&self, connector_id: i32) -> Result<HashSet<String>, ReachabilityError> {
        Ok(ingress_publications::Entity::find()
            .filter(ingress_publications::Column::ConnectorId.eq(connector_id))
            .filter(ingress_publications::Column::State.eq(PUBLISHED))
            .all(self.db())
            .await?
            .into_iter()
            .map(|p| p.hostname.to_ascii_lowercase())
            .collect())
    }

    /// One pass: bring the generated rows in line with the environments.
    pub async fn pass(&self) -> Result<RowChanges, ReachabilityError> {
        let Some(connector) = ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::EnvironmentHosts.eq(true))
            .one(self.db())
            .await?
        else {
            return Ok(RowChanges::default());
        };
        // A tunnel in trouble keeps what it has; nothing new is generated.
        if connector.status != CONNECTOR_READY {
            return Ok(RowChanges::default());
        }
        let snap = self.snapshot(connector).await?;
        let desired = self.desired(&snap).await;
        let published = self.published(snap.connector.id).await?;
        let generate_new = !self.preview_usable().await;
        let changes = plan_rows(&snap.envs, &snap.rows, &desired, &published, generate_new);
        self.apply(snap.connector.id, &changes).await?;
        Ok(changes)
    }

    async fn apply(
        &self,
        connector_id: i32,
        changes: &RowChanges,
    ) -> Result<(), ReachabilityError> {
        if changes.insert.is_empty() && changes.delete.is_empty() {
            return Ok(());
        }
        let txn = self.db().begin().await?;
        let mut skipped = 0usize;
        for (environment_id, host) in &changes.insert {
            // Under the hostname lock custom domains, environment domains
            // and status pages claim names under: the name read as free in
            // the snapshot may have been claimed since.
            temps_entities::status_page_domains::lock_hostname_claim(&txn, host).await?;
            if let Some(holder) = hostname_holder(&txn, host).await? {
                warn!(
                    connector_id,
                    environment_id,
                    %host,
                    holder,
                    "not generating a host claimed since the pass began; the next pass picks another"
                );
                skipped += 1;
                continue;
            }
            environment_domains::ActiveModel {
                environment_id: Set(*environment_id),
                domain: Set(host.clone()),
                audience: Set(PUBLIC.to_string()),
                generated_by_connector_id: Set(Some(connector_id)),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
        }
        if !changes.delete.is_empty() {
            // Only rows this tunnel generated, whatever the ids say.
            environment_domains::Entity::delete_many()
                .filter(environment_domains::Column::Id.is_in(changes.delete.iter().copied()))
                .filter(environment_domains::Column::GeneratedByConnectorId.eq(connector_id))
                .exec(&txn)
                .await?;
        }
        notify_routes(&txn).await;
        txn.commit().await?;
        info!(
            connector_id,
            added = changes.insert.len() - skipped,
            withdrawn = changes.delete.len(),
            "updated tunnel environment hosts"
        );
        Ok(())
    }

    /// What `connector_id` publishes, or would publish once turned on.
    pub async fn list(
        &self,
        connector_id: i32,
    ) -> Result<EnvironmentHostsResponse, ReachabilityError> {
        let connector = self.connector(connector_id).await?;
        let enabled_elsewhere = ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::EnvironmentHosts.eq(true))
            .filter(ingress_connectors::Column::Id.ne(connector_id))
            .one(self.db())
            .await?
            .map(|c| c.name);
        let preview_domain_usable = self.preview_usable().await;
        let snap = self.snapshot(connector).await?;
        let domain = snap.connector.domain.clone();
        let publications: HashMap<String, ingress_publications::Model> =
            ingress_publications::Entity::find()
                .filter(ingress_publications::Column::ConnectorId.eq(connector_id))
                .all(self.db())
                .await?
                .into_iter()
                .map(|p| (p.hostname.to_ascii_lowercase(), p))
                .collect();
        let mut hosts = Vec::new();
        let mut refused = Vec::new();
        for env in &snap.envs {
            if let Some(exclusion) = env.excluded {
                if exclusion != Exclusion::Deleted {
                    refused.push(EnvironmentHostRefusal {
                        environment_id: env.environment_id,
                        project_slug: env.project_slug.clone(),
                        environment_name: env.environment_name.clone(),
                        reason: exclusion.reason().to_string(),
                    });
                }
                continue;
            }
            let label = host_label(env, snap.production_template());
            let natural = natural_host(&label, &domain);
            let rows: Vec<&GeneratedRow> = snap
                .rows
                .iter()
                .filter(|r| r.environment_id == env.environment_id)
                .collect();
            if rows.is_empty() {
                hosts.push(EnvironmentHostView {
                    environment_id: env.environment_id,
                    project_slug: env.project_slug.clone(),
                    environment_name: env.environment_name.clone(),
                    note: (snap.is_taken(&natural)
                        || snap
                            .holder
                            .get(&natural)
                            .is_some_and(|e| *e != env.environment_id))
                    .then(|| {
                        format!(
                            "{natural} is already in use, so it will get {}",
                            fallback_host(&label, &domain, env.environment_id)
                        )
                    }),
                    hostname: natural,
                    state: "planned".to_string(),
                    last_error: None,
                });
                continue;
            }
            for row in rows {
                let publication = publications.get(&row.domain);
                hosts.push(EnvironmentHostView {
                    environment_id: env.environment_id,
                    project_slug: env.project_slug.clone(),
                    environment_name: env.environment_name.clone(),
                    hostname: row.domain.clone(),
                    state: publication
                        .map(|p| p.state.clone())
                        .unwrap_or_else(|| "pending".to_string()),
                    last_error: publication.and_then(|p| p.last_error.clone()),
                    note: (row.domain != natural).then(|| {
                        format!(
                            "{natural} is held by a DNS record Temps does not own, or by another \
                             address here, so this environment uses {} instead",
                            row.domain
                        )
                    }),
                });
            }
        }
        Ok(EnvironmentHostsResponse {
            connector_id,
            connector_name: snap.connector.name.clone(),
            domain,
            enabled: snap.connector.environment_hosts,
            confirm_token: confirm_token(connector_id, snap.connector.environment_hosts, &hosts),
            hosts,
            refused,
            enabled_elsewhere,
            preview_domain_usable,
            warning: PUBLIC_NAMES_WARNING.to_string(),
        })
    }

    /// Turn environment hosts on or off for `connector_id`. Both directions
    /// need `confirm`, the `confirm_token` of the list that was shown: on
    /// publishes every host listed, off withdraws them.
    pub async fn set(
        &self,
        connector_id: i32,
        request: &SetEnvironmentHostsRequest,
    ) -> Result<EnvironmentHostsResponse, ReachabilityError> {
        let current = self.list(connector_id).await?;
        if current.enabled == request.enabled {
            return Ok(current);
        }
        let connector = self.connector(connector_id).await?;
        check_confirmation(&current, request)?;
        set_environment_hosts(self.db(), &connector, request.enabled).await?;
        // The hosts are generated and published by the reconciler, which
        // runs the pass under its lock: never two passes at once.
        let connectors = self.reachability.deps.connectors.clone();
        tokio::spawn(async move { connectors.reconcile_all().await });
        self.list(connector_id).await
    }
}

/// Whether `request` confirms exactly the list in `current`.
fn check_confirmation(
    current: &EnvironmentHostsResponse,
    request: &SetEnvironmentHostsRequest,
) -> Result<(), ReachabilityError> {
    let what = if request.enabled {
        "on publishes a public name for every environment listed"
    } else {
        "off withdraws every name it published"
    };
    match request.confirm.as_deref() {
        Some(token) if token == current.confirm_token => Ok(()),
        None => Err(ReachabilityError::Validation(format!(
            "Turning environment hosts {what} for tunnel {}; list them with GET \
             /ingress/connectors/{}/environment-hosts and send its confirm_token as confirm",
            current.connector_name, current.connector_id
        ))),
        Some(_) => Err(ReachabilityError::Conflict(format!(
            "The environment hosts of tunnel {} changed since they were listed; list them \
             again with GET /ingress/connectors/{}/environment-hosts and confirm the new list",
            current.connector_name, current.connector_id
        ))),
    }
}

/// Set the flag, withdrawing the generated rows when it goes off. Used by
/// the endpoint and by the ADR 064 run that turns it on (and its undo).
pub async fn set_environment_hosts(
    db: &sea_orm::DatabaseConnection,
    connector: &ingress_connectors::Model,
    enabled: bool,
) -> Result<(), ReachabilityError> {
    let txn = db.begin().await?;
    if enabled {
        if let Some(other) = ingress_connectors::Entity::find()
            .filter(ingress_connectors::Column::EnvironmentHosts.eq(true))
            .filter(ingress_connectors::Column::Id.ne(connector.id))
            .one(&txn)
            .await?
        {
            return Err(ReachabilityError::Conflict(format!(
                "Tunnel {} already publishes environment hosts; turn it off there before \
                 turning it on for tunnel {}",
                other.name, connector.name
            )));
        }
    } else {
        environment_domains::Entity::delete_many()
            .filter(environment_domains::Column::GeneratedByConnectorId.eq(connector.id))
            .exec(&txn)
            .await?;
        notify_routes(&txn).await;
    }
    let mut update: ingress_connectors::ActiveModel = connector.clone().into();
    update.environment_hosts = Set(enabled);
    // D2's cached answer is keyed on it.
    update.updated_at = Set(chrono::Utc::now());
    update
        .update(&txn)
        .await
        .map_err(|error| match error.sql_err() {
            // Another tunnel turned it on between the check and this write: the
            // partial unique index keeps it to one.
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                ReachabilityError::Conflict(format!(
                    "Another tunnel turned environment hosts on first; only one tunnel can \
                 publish them, so tunnel {} was left off",
                    connector.name
                ))
            }
            _ => ReachabilityError::from(error),
        })?;
    txn.commit().await?;
    info!(
        connector_id = connector.id,
        enabled, "tunnel environment hosts switched"
    );
    Ok(())
}

/// What already holds `host`, if anything: a custom domain, an environment
/// domain (generated or not) or a status page. Compared case-insensitively.
async fn hostname_holder<C: ConnectionTrait>(
    conn: &C,
    host: &str,
) -> Result<Option<&'static str>, sea_orm::DbErr> {
    use sea_orm::sea_query::{Expr, Func};
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if project_custom_domains::Entity::find()
        .filter(
            Expr::expr(Func::lower(Expr::col(
                project_custom_domains::Column::Domain,
            )))
            .eq(host.as_str()),
        )
        .one(conn)
        .await?
        .is_some()
    {
        return Ok(Some("a custom domain"));
    }
    if environment_domains::Entity::find()
        .filter(
            Expr::expr(Func::lower(Expr::col(environment_domains::Column::Domain)))
                .eq(host.as_str()),
        )
        .one(conn)
        .await?
        .is_some()
    {
        return Ok(Some("an environment domain"));
    }
    if temps_entities::status_page_domains::attached_hostname_conflict(conn, &host).await? {
        return Ok(Some("a status page"));
    }
    Ok(None)
}

/// Tell every proxy the route table changed.
async fn notify_routes<C: ConnectionTrait>(db: &C) {
    if let Err(error) = db
        .execute_unprepared("NOTIFY route_table_changes, 'environment_hosts'")
        .await
    {
        warn!(%error, "could not notify route table listeners");
    }
}

#[async_trait]
impl EnvironmentHostSync for EnvironmentHostService {
    async fn sync(&self) {
        if let Err(error) = self.pass().await {
            warn!(%error, "tunnel environment hosts pass failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector(id: i32, environment_hosts: bool) -> ingress_connectors::Model {
        ingress_connectors::Model {
            id,
            name: format!("tunnel-{id}"),
            provider: "cloudflare_tunnel".into(),
            dns_provider_id: Some(3),
            domain: "tunnel.example.com".into(),
            external_id: None,
            settings: serde_json::json!({}),
            secret_encrypted: None,
            edge_port: 443,
            auto_publish: false,
            environment_hosts,
            status: CONNECTOR_READY.into(),
            last_error: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn statements(db: sea_orm::DatabaseConnection) -> Vec<String> {
        db.into_transaction_log()
            .into_iter()
            .map(|t| format!("{t:?}"))
            .collect()
    }

    /// Q3: at most one tunnel publishes environment hosts; a second is
    /// refused before anything is written.
    #[tokio::test]
    async fn a_second_tunnel_cannot_turn_environment_hosts_on() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![connector(2, true)]])
            .into_connection();

        let error = set_environment_hosts(&db, &connector(1, false), true)
            .await
            .unwrap_err();

        assert!(
            matches!(&error, ReachabilityError::Conflict(message) if message.contains("tunnel-2")),
            "{error}"
        );
        let log = statements(db);
        assert!(log.iter().all(|s| !s.contains("UPDATE")), "{log:?}");
    }

    #[tokio::test]
    async fn turning_environment_hosts_on_sets_only_the_flag() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<ingress_connectors::Model>::new()])
            .append_query_results([vec![connector(1, true)]])
            .into_connection();

        set_environment_hosts(&db, &connector(1, false), true)
            .await
            .unwrap();

        let log = statements(db).join("\n");
        assert!(log.contains("UPDATE \\\"ingress_connectors\\\""), "{log}");
        assert!(!log.contains("DELETE"), "turning on removes nothing: {log}");
    }

    /// Opting out withdraws what this tunnel generated, and only that: a row
    /// the user added carries no connector and is never matched.
    #[tokio::test]
    async fn turning_environment_hosts_off_removes_only_this_tunnels_rows() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                },
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
            ])
            .append_query_results([vec![connector(1, false)]])
            .into_connection();

        set_environment_hosts(&db, &connector(1, true), false)
            .await
            .unwrap();

        let log = statements(db).join("\n");
        assert!(
            log.contains(
                "DELETE FROM \\\"environment_domains\\\" WHERE \\\"environment_domains\\\".\\\"generated_by_connector_id\\\" = $1"
            ),
            "{log}"
        );
        assert!(log.contains("NOTIFY route_table_changes"), "{log}");
    }

    fn env(id: i32, label: &str) -> EnvironmentFacts {
        EnvironmentFacts {
            environment_id: id,
            project_slug: "shop".into(),
            environment_name: if label.ends_with("production") {
                "production".into()
            } else {
                "staging".into()
            },
            label: label.into(),
            is_production: label.ends_with("production"),
            excluded: None,
        }
    }

    fn row(id: i32, environment_id: i32, domain: &str) -> GeneratedRow {
        GeneratedRow {
            id,
            environment_id,
            domain: domain.into(),
        }
    }

    #[test]
    fn hosts_are_the_label_under_the_tunnel_zone() {
        assert_eq!(
            natural_host("shop-staging", "Tunnel.Example.com."),
            "shop-staging.tunnel.example.com"
        );
        let staging = env(2, "shop-staging");
        assert_eq!(host_label(&staging, Some("{project}")), "shop-staging");
    }

    #[test]
    fn production_follows_the_production_template() {
        let production = env(1, "shop-production");
        assert_eq!(host_label(&production, Some("{project}")), "shop");
        assert_eq!(host_label(&production, Some("  ")), "shop-production");
        assert_eq!(host_label(&production, None), "shop-production");
    }

    #[test]
    fn the_fallback_is_a_stable_short_hash_name() {
        let first = fallback_host("shop-staging", "tunnel.example.com", 2);
        assert!(first.starts_with("shop-staging-"));
        assert!(first.ends_with(".tunnel.example.com"));
        assert_eq!(
            first,
            fallback_host("shop-staging", "tunnel.example.com", 2)
        );
        assert_ne!(
            first,
            fallback_host("shop-staging", "tunnel.example.com", 3)
        );
        assert!(is_generated_name(
            &first,
            "shop-staging",
            "tunnel.example.com",
            2
        ));
        assert!(is_generated_name(
            "SHOP-STAGING.tunnel.example.com",
            "shop-staging",
            "tunnel.example.com",
            2
        ));
        assert!(!is_generated_name(
            "shop-old.tunnel.example.com",
            "shop-staging",
            "tunnel.example.com",
            2
        ));
    }

    #[test]
    fn every_eligible_environment_gets_one_row() {
        let envs = [env(1, "shop-production"), env(2, "shop-staging")];
        let desired = HashMap::from([
            (1, "shop-production.t.example.com".to_string()),
            (2, "shop-staging.t.example.com".to_string()),
        ]);
        let changes = plan_rows(&envs, &[], &desired, &HashSet::new(), true);
        assert_eq!(
            changes.insert,
            vec![
                (1, "shop-production.t.example.com".to_string()),
                (2, "shop-staging.t.example.com".to_string())
            ]
        );
        assert!(changes.delete.is_empty());
    }

    #[test]
    fn a_rename_publishes_the_new_host_before_withdrawing_the_old() {
        let envs = [env(2, "shop-qa")];
        let rows = [row(10, 2, "shop-staging.t.example.com")];
        let desired = HashMap::from([(2, "shop-qa.t.example.com".to_string())]);

        // First pass: the new host is added, the old one stays.
        let first = plan_rows(&envs, &rows, &desired, &HashSet::new(), true);
        assert_eq!(first.insert, vec![(2, "shop-qa.t.example.com".to_string())]);
        assert!(
            first.delete.is_empty(),
            "the old host stays until the new one is live"
        );

        // Once the new host is published, the old row goes.
        let rows = [
            row(10, 2, "shop-staging.t.example.com"),
            row(11, 2, "shop-qa.t.example.com"),
        ];
        let published = HashSet::from(["shop-qa.t.example.com".to_string()]);
        let second = plan_rows(&envs, &rows, &desired, &published, true);
        assert!(second.insert.is_empty());
        assert_eq!(second.delete, vec![10]);
    }

    #[test]
    fn deleted_and_restricted_environments_lose_their_rows() {
        let mut deleted = env(1, "shop-production");
        deleted.excluded = Some(Exclusion::Deleted);
        let mut private = env(2, "shop-staging");
        private.excluded = Some(Exclusion::Restricted);
        let rows = [
            row(10, 1, "shop-production.t.example.com"),
            row(11, 2, "shop-staging.t.example.com"),
            // An environment that no longer exists at all.
            row(12, 9, "gone.t.example.com"),
        ];
        let desired = HashMap::from([
            (1, "shop-production.t.example.com".to_string()),
            (2, "shop-staging.t.example.com".to_string()),
        ]);
        let changes = plan_rows(&[deleted, private], &rows, &desired, &HashSet::new(), true);
        assert!(
            changes.insert.is_empty(),
            "never generates for an excluded environment"
        );
        assert_eq!(changes.delete, vec![10, 11, 12]);
    }

    #[test]
    fn any_non_public_address_keeps_the_environment_off_the_tunnel() {
        let restricted = restricted_environments(
            [
                // A tailnet name for listed people only (ADR 065 D6).
                (1, "tailnet_users", false),
                // A tailnet name through Serve is `private`.
                (2, "private", false),
                (3, "public", false),
                // Temps' own generated row says nothing about the audience.
                (4, "public", true),
                (5, "signed_in", false),
                (6, "unsupported", false),
            ]
            .into_iter(),
            [(7, "tailnet_users"), (8, "public")].into_iter(),
        );
        assert_eq!(restricted, HashSet::from([1, 2, 5, 6, 7]));
    }

    #[test]
    fn a_usable_preview_domain_keeps_existing_hosts_and_stops_new_ones() {
        let envs = [env(1, "shop-production"), env(2, "shop-staging")];
        let rows = [row(10, 1, "shop-production.t.example.com")];
        let desired = HashMap::from([
            (1, "shop-production.t.example.com".to_string()),
            (2, "shop-staging.t.example.com".to_string()),
        ]);
        let changes = plan_rows(&envs, &rows, &desired, &HashSet::new(), false);
        assert_eq!(changes, RowChanges::default());
    }

    #[test]
    fn only_generated_rows_are_ever_deleted() {
        // `plan_rows` is only ever handed generated rows, and every id it
        // returns comes from that list: a user's row cannot be named.
        let envs = [env(1, "shop-production")];
        let rows = [
            row(10, 1, "old.t.example.com"),
            row(11, 1, "shop-production.t.example.com"),
        ];
        let desired = HashMap::from([(1, "shop-production.t.example.com".to_string())]);
        let published = HashSet::from(["shop-production.t.example.com".to_string()]);
        let changes = plan_rows(&envs, &rows, &desired, &published, true);
        let ids: HashSet<i32> = rows.iter().map(|r| r.id).collect();
        assert!(changes.delete.iter().all(|id| ids.contains(id)));
        assert_eq!(changes.delete, vec![10]);
    }

    #[test]
    fn a_duplicate_generated_row_is_collapsed() {
        let envs = [env(1, "shop-production")];
        let rows = [
            row(10, 1, "shop-production.t.example.com"),
            row(11, 1, "shop-production.t.example.com"),
        ];
        let desired = HashMap::from([(1, "shop-production.t.example.com".to_string())]);
        let changes = plan_rows(&envs, &rows, &desired, &HashSet::new(), true);
        assert_eq!(changes.delete, vec![11]);
    }

    const ZONE: &str = "t.example.com";

    fn assign(envs: &[EnvironmentFacts], rows: &[GeneratedRow]) -> HashMap<i32, String> {
        assign_hosts(envs, rows, None, ZONE, &|_| false, &|_| true)
    }

    /// Two environments with one label never share a host, whichever comes
    /// first, and the one already holding it keeps it.
    #[test]
    fn two_environments_never_get_one_host_in_either_order() {
        let natural = natural_host("shop-staging", ZONE);
        for envs in [
            vec![env(1, "shop-staging"), env(2, "shop-staging")],
            vec![env(2, "shop-staging"), env(1, "shop-staging")],
        ] {
            let desired = assign(&envs, &[]);
            assert_eq!(desired.len(), 2);
            assert_ne!(desired[&1], desired[&2]);
            assert!(desired.values().any(|h| *h == natural));

            // Environment 2 already holds the name: it keeps it.
            let rows = [row(10, 2, &natural)];
            let desired = assign(&envs, &rows);
            assert_eq!(desired[&2], natural);
            assert_eq!(desired[&1], fallback_host("shop-staging", ZONE, 1));
        }
    }

    /// A renamed environment keeps its old name until the new one is live;
    /// meanwhile nobody else is given it.
    #[test]
    fn a_renamed_environments_old_name_is_not_handed_out() {
        let old = natural_host("shop-staging", ZONE);
        let rows = [row(10, 2, &old)];
        let envs = [env(1, "shop-staging"), env(2, "shop-qa")];
        let desired = assign(&envs, &rows);
        assert_eq!(desired[&2], natural_host("shop-qa", ZONE));
        assert_eq!(desired[&1], fallback_host("shop-staging", ZONE, 1));
        let changes = plan_rows(&envs, &rows, &desired, &HashSet::new(), true);
        assert!(
            changes.delete.is_empty(),
            "the old name stays until shop-qa is live"
        );
    }

    /// Two rows that already hold one name: the oldest keeps it, the other
    /// goes at once and its environment gets the fallback.
    #[test]
    fn a_name_held_twice_is_given_to_the_oldest_row_only() {
        let name = natural_host("shop-staging", ZONE);
        let envs = [env(2, "shop-staging"), env(1, "shop-staging")];
        let rows = [row(11, 2, &name), row(10, 1, &name)];
        let desired = assign(&envs, &rows);
        assert_eq!(desired[&1], name);
        assert_eq!(desired[&2], fallback_host("shop-staging", ZONE, 2));
        let changes = plan_rows(&envs, &rows, &desired, &HashSet::new(), true);
        assert_eq!(changes.delete, vec![11]);
        assert_eq!(
            changes.insert,
            vec![(2, fallback_host("shop-staging", ZONE, 2))]
        );
    }

    /// A name held elsewhere (another address, Temps itself, a foreign
    /// record) gets the fallback; a host the zone cannot carry gets nothing.
    #[test]
    fn a_taken_name_gets_the_fallback_and_an_unpublishable_one_nothing() {
        let envs = [env(1, "www")];
        let natural = natural_host("www", ZONE);
        let desired = assign_hosts(&envs, &[], None, ZONE, &|h| h == natural, &|_| true);
        assert_eq!(desired[&1], fallback_host("www", ZONE, 1));
        let desired = assign_hosts(&envs, &[], None, ZONE, &|_| true, &|_| true);
        assert!(desired.is_empty(), "both names taken");
        let desired = assign_hosts(&envs, &[], None, ZONE, &|_| false, &|_| false);
        assert!(desired.is_empty());
    }

    /// Labels are one ASCII DNS label of at most 63 characters, whatever the
    /// branch name or an old subdomain held.
    #[test]
    fn host_labels_are_one_short_ascii_label() {
        for raw in [
            "shop-feature-\u{0430}dd-login",
            "shop.old.subdomain",
            &"shop-a-very-long-branch-name".repeat(4),
        ] {
            let label = host_label(&env(3, raw), None);
            assert!(label.len() <= 63, "{label}");
            assert!(!label.contains('.'), "{label}");
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{label}"
            );
        }
    }

    fn listed(enabled: bool, hosts: &[(i32, &str)]) -> EnvironmentHostsResponse {
        let hosts: Vec<EnvironmentHostView> = hosts
            .iter()
            .map(|(id, host)| EnvironmentHostView {
                environment_id: *id,
                project_slug: "shop".into(),
                environment_name: "staging".into(),
                hostname: (*host).into(),
                state: "planned".into(),
                last_error: None,
                note: None,
            })
            .collect();
        EnvironmentHostsResponse {
            connector_id: 1,
            connector_name: "edge".into(),
            domain: ZONE.into(),
            enabled,
            confirm_token: confirm_token(1, enabled, &hosts),
            hosts,
            refused: Vec::new(),
            enabled_elsewhere: None,
            preview_domain_usable: false,
            warning: PUBLIC_NAMES_WARNING.into(),
        }
    }

    /// The confirmation names the list that was shown: another list, or none,
    /// is refused.
    #[test]
    fn confirming_is_bound_to_the_list_that_was_shown() {
        let shown = listed(false, &[(1, "shop-staging.t.example.com")]);
        let request = |confirm: Option<&str>| SetEnvironmentHostsRequest {
            enabled: true,
            confirm: confirm.map(str::to_string),
        };
        assert!(check_confirmation(&shown, &request(Some(&shown.confirm_token))).is_ok());
        assert!(matches!(
            check_confirmation(&shown, &request(None)),
            Err(ReachabilityError::Validation(_))
        ));
        // A new environment appeared since the list was shown.
        let now = listed(
            false,
            &[
                (1, "shop-staging.t.example.com"),
                (2, "shop-qa.t.example.com"),
            ],
        );
        assert_ne!(now.confirm_token, shown.confirm_token);
        assert!(matches!(
            check_confirmation(&now, &request(Some(&shown.confirm_token))),
            Err(ReachabilityError::Conflict(_))
        ));
        // The order hosts are listed in does not matter; the on/off state does.
        let reordered = listed(
            false,
            &[
                (2, "shop-qa.t.example.com"),
                (1, "shop-staging.t.example.com"),
            ],
        );
        assert_eq!(reordered.confirm_token, now.confirm_token);
        assert_ne!(
            listed(true, &[(1, "shop-staging.t.example.com")]).confirm_token,
            shown.confirm_token
        );
    }

    /// A name claimed after the snapshot is not generated: the insert path
    /// re-reads custom domains, environment domains and status pages.
    #[tokio::test]
    async fn a_name_claimed_since_the_snapshot_is_held() {
        let claimed = environment_domains::Model {
            id: 4,
            environment_id: 9,
            domain: "shop-staging.t.example.com".into(),
            created_at: chrono::Utc::now(),
            audience: PUBLIC.into(),
            private_range: None,
            private_node_id: None,
            private_listener: None,
            generated_by_connector_id: None,
            private_via: None,
            private_service: None,
            gate: None,
            gate_allow: None,
            gate_not_before: None,
            gate_access_provider_id: None,
            gate_access_app_id: None,
            gate_access_aud: None,
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<project_custom_domains::Model>::new()])
            .append_query_results([vec![claimed]])
            .into_connection();
        assert_eq!(
            hostname_holder(&db, "Shop-Staging.t.example.com")
                .await
                .unwrap(),
            Some("an environment domain")
        );
        let log = statements(db).join("\n");
        assert!(log.contains("LOWER"), "{log}");

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<project_custom_domains::Model>::new()])
            .append_query_results([Vec::<environment_domains::Model>::new()])
            .append_query_results([Vec::<temps_entities::status_page_domains::Model>::new()])
            .into_connection();
        assert_eq!(
            hostname_holder(&db, "free.t.example.com").await.unwrap(),
            None
        );
    }
}
