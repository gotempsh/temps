// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ingress endpoints compiled into the route snapshot (ADR-055 §11, Phase 8).
//!
//! Every environment whose `ingress_mode` is not `legacy` gets a compiled
//! endpoint, built here at snapshot time and published with the routes it
//! belongs to. Its access is, in order:
//!
//! 1. the policies **derived** from the access settings the legacy chain
//!    reads (see [`derived_policies`]), in the order the legacy chain checks
//!    them, so a derived endpoint decides exactly what the legacy chain
//!    decides; then
//! 2. the policies stored for the endpoint in `ingress_access_policies`.
//!
//! Derived policies are recomputed from the settings on every reload rather
//! than copied into rows: the settings keep one source of truth (the one the
//! console already edits), and a shadow-mode disagreement can only mean the
//! two evaluators differ, never that a copy drifted.
//!
//! What stays outside the model, evaluated by the proxy before it: the
//! instance blocklist (every host), a custom request-policy provider, and
//! the per-environment connection cap (a capacity limit, not access).
//!
//! Nothing here runs per request. Compiling is O(non-legacy environments)
//! and happens inside a route reload, which is already O(routes).

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;

use ipnetwork::IpNetwork;
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, DbErr, EntityTrait, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use temps_core::RateLimitSettings;
use temps_entities::ingress_mode::{IngressMode, SandboxPreviewAccess};
use temps_entities::{
    environments, ingress_access_policies, ingress_endpoints, ingress_login_grants,
    project_ip_access_rules, projects, sandboxes, users,
};
use temps_workload_ingress::{
    AccessPolicy, CompiledEndpoint, CompiledRoutes, Endpoint, EndpointProtocol, EndpointTarget,
    IpRule, ProductKind, ProductRef, RateLimit, RateLimitScope, Revocations, UnresolvedClient,
};
use tracing::{error, warn};

/// The permission a `TempsLogin` grant for a sandbox preview carries: the
/// console signs it only for a user who may read the sandbox.
pub const SANDBOX_PREVIEW_PERMISSION: &str = SandboxPreviewAccess::LOGIN_PERMISSION;

/// The permission a `TempsLogin` grant for an environment carries when the
/// policy does not name one.
pub const ENVIRONMENT_LOGIN_PERMISSION: &str = "projects:read";

/// Upper bound on entries in one derived address list. Mirrors the bound
/// the services enforce on write; a hand-edited row beyond it is truncated
/// with an error log rather than growing the snapshot without limit.
const MAX_DERIVED_ENTRIES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum IngressLoadError {
    #[error("Failed to load ingress {what} for the route snapshot: {source}")]
    Database {
        what: &'static str,
        #[source]
        source: DbErr,
    },
}

fn db_err(what: &'static str) -> impl FnOnce(DbErr) -> IngressLoadError {
    move |source| IngressLoadError::Database { what, source }
}

/// The access settings of one environment, as the legacy chain reads them.
#[derive(Debug, Clone, Copy)]
pub struct EnvironmentAccess<'a> {
    pub environment: &'a environments::Model,
    pub project: &'a projects::Model,
    /// `AppSettings.rate_limiting`.
    pub instance_rate_limiting: &'a RateLimitSettings,
    /// The `project_ip_access_rules` rows of the environment's project.
    pub ip_rules: &'a [project_ip_access_rules::Model],
}

/// The environment's owner reference.
pub fn environment_owner(environment_id: i32) -> ProductRef {
    ProductRef {
        kind: ProductKind::Environment,
        id: i64::from(environment_id),
    }
}

/// The sandbox's owner reference.
pub fn sandbox_owner(sandbox_id: i32) -> ProductRef {
    ProductRef {
        kind: ProductKind::Sandbox,
        id: i64::from(sandbox_id),
    }
}

/// The policies the legacy chain enforces for this environment, in the
/// order it checks them:
///
/// 1. the project IP gate (`project_ip_access_rules`, environment override
///    first), failing closed for an unresolved client;
/// 2. the rate-limit blacklists (instance and project), which only ever see
///    a resolved client;
/// 3. the per-client rate limit, with its whitelist exempt;
/// 4. attack mode, as a captcha;
/// 5. the password wall, keyed by the environment id.
pub fn derived_policies(access: &EnvironmentAccess<'_>) -> Vec<AccessPolicy> {
    let mut policies = Vec::new();
    if let Some(policy) = project_ip_policy(access) {
        policies.push(policy);
    }
    rate_limit_policies(access, &mut policies);
    let attack_mode = access
        .environment
        .attack_mode
        .unwrap_or(access.project.attack_mode);
    if attack_mode {
        policies.push(AccessPolicy::Captcha);
    }
    if password_hash(access.environment).is_some() {
        policies.push(AccessPolicy::Password {
            password_ref: i64::from(access.environment.id),
        });
    }
    policies
}

/// The password wall's hash, when the wall is on.
pub fn password_hash(environment: &environments::Model) -> Option<&str> {
    environment
        .deployment_config
        .as_ref()
        .and_then(|dc| dc.security.as_ref())
        .and_then(|s| s.password_protection.as_ref())
        .filter(|pp| pp.enabled)
        .map(|pp| pp.password_hash.as_str())
}

/// Every address, both families: what "refuse everyone" denies.
fn deny_everyone() -> AccessPolicy {
    AccessPolicy::IpRules {
        rules: vec![
            IpRule {
                cidr: "0.0.0.0/0".into(),
                allow: false,
            },
            IpRule {
                cidr: "::/0".into(),
                allow: false,
            },
        ],
        unresolved: UnresolvedClient::Deny,
    }
}

/// An entry of a `project_ip_access_rules` list, read exactly as the proxy's
/// IP gate reads it: trimmed, at most 64 characters, no host bits, an
/// IPv4-mapped IPv6 block of prefix 96 or more folded into IPv4.
fn gate_entry(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    let network: IpNetwork = trimmed.parse().ok()?;
    if network.ip() != network.network() {
        return None;
    }
    if let IpNetwork::V6(v6) = network {
        if v6.prefix() >= 96 {
            if let Some(v4) = v6.network().to_ipv4_mapped() {
                return Some(format!("{}/{}", v4, v6.prefix() - 96));
            }
        }
    }
    Some(format!("{}/{}", network.network(), network.prefix()))
}

fn project_ip_policy(access: &EnvironmentAccess<'_>) -> Option<AccessPolicy> {
    let environment_id = access.environment.id;
    let project_id = access.environment.project_id;
    let rows = access
        .ip_rules
        .iter()
        .filter(|row| row.project_id == project_id);
    let mut project_row = None;
    let mut environment_row = None;
    for row in rows {
        match row.environment_id {
            Some(id) if id == environment_id => environment_row = Some(row),
            None => project_row = Some(row),
            Some(_) => {}
        }
    }
    let row = environment_row.or(project_row)?;
    let allow = match row.mode.as_str() {
        "disabled" => return None,
        "allow" => true,
        "deny" => false,
        other => {
            error!(
                rule_id = row.id,
                project_id,
                environment_id,
                mode = other,
                "Unknown IP access mode; the compiled endpoint refuses every client for this scope"
            );
            return Some(deny_everyone());
        }
    };
    let mut rules = Vec::with_capacity(row.cidrs.0.len());
    let mut unparsable = 0usize;
    for entry in &row.cidrs.0 {
        match gate_entry(&entry.cidr) {
            Some(cidr) => rules.push(IpRule { cidr, allow }),
            None => unparsable += 1,
        }
    }
    if allow {
        if rules.is_empty() {
            return Some(deny_everyone());
        }
    } else {
        if unparsable > 0 {
            return Some(deny_everyone());
        }
        if rules.is_empty() {
            return None;
        }
    }
    Some(AccessPolicy::IpRules {
        rules,
        unresolved: UnresolvedClient::Deny,
    })
}

/// An entry of a rate-limit white/blacklist, read exactly as the proxy's
/// rate limiter reads it: an entry with `/` is a network whose host bits are
/// ignored, anything else a single address (IPv4-mapped read as IPv4).
/// Unparsable entries match nothing.
fn listed_entry(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    if entry.contains('/') {
        let network = IpNetwork::from_str(entry).ok()?;
        Some(format!("{}/{}", network.network(), network.prefix()))
    } else {
        let addr = IpAddr::from_str(entry).ok()?;
        let addr = match addr {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(addr),
            v4 => v4,
        };
        Some(match addr {
            IpAddr::V4(v4) => format!("{v4}/32"),
            IpAddr::V6(v6) => format!("{v6}/128"),
        })
    }
}

fn listed(entries: &[&[String]]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for list in entries {
        for entry in list.iter() {
            if out.len() >= MAX_DERIVED_ENTRIES {
                error!(
                    limit = MAX_DERIVED_ENTRIES,
                    "Rate-limit address list exceeds the compiled limit; extra entries ignored"
                );
                return out;
            }
            if let Some(cidr) = listed_entry(entry) {
                if !out.contains(&cidr) {
                    out.push(cidr);
                }
            }
        }
    }
    out
}

fn rate_limit_policies(access: &EnvironmentAccess<'_>, policies: &mut Vec<AccessPolicy>) {
    let instance = access.instance_rate_limiting;
    let project_config = access.project.deployment_config.clone().unwrap_or_default();
    let effective = access
        .environment
        .get_effective_deployment_config(&project_config);
    let project_rl = effective
        .security
        .as_ref()
        .filter(|security| security.enabled == Some(true))
        .and_then(|security| security.rate_limiting.as_ref());
    let instance_blacklist: &[String] = if instance.enabled {
        &instance.blacklist_ips
    } else {
        &[]
    };

    let (blacklist, whitelist, per_minute, per_hour, scope) = match project_rl {
        Some(rl) => (
            listed(&[instance_blacklist, &rl.blacklist_ips]),
            listed(&[&rl.whitelist_ips]),
            rl.max_requests_per_minute
                .unwrap_or(instance.max_requests_per_minute),
            rl.max_requests_per_hour
                .unwrap_or(instance.max_requests_per_hour),
            RateLimitScope::Endpoint,
        ),
        None if instance.enabled => (
            listed(&[&instance.blacklist_ips]),
            listed(&[&instance.whitelist_ips]),
            instance.max_requests_per_minute,
            instance.max_requests_per_hour,
            RateLimitScope::Instance,
        ),
        None => return,
    };

    if !blacklist.is_empty() {
        policies.push(AccessPolicy::IpRules {
            rules: blacklist
                .into_iter()
                .map(|cidr| IpRule { cidr, allow: false })
                .collect(),
            // The legacy rate limiter only ever sees a resolved client.
            unresolved: UnresolvedClient::Admit,
        });
    }
    let mut limits = Vec::with_capacity(2);
    if per_minute > 0 {
        limits.push(RateLimit {
            requests: per_minute,
            per_secs: 60,
        });
    }
    if per_hour > 0 {
        limits.push(RateLimit {
            requests: per_hour,
            per_secs: 3600,
        });
    }
    if !limits.is_empty() {
        policies.push(AccessPolicy::RateLimit {
            limits,
            exempt: whitelist,
            scope,
        });
    }
}

/// One environment's endpoint: the derived policies, then `stored`.
pub fn environment_endpoint(
    endpoint_id: i64,
    access: &EnvironmentAccess<'_>,
    path_prefix: Option<String>,
    stored: impl IntoIterator<Item = AccessPolicy>,
) -> Endpoint {
    let mut policies = derived_policies(access);
    policies.extend(stored);
    Endpoint {
        id: endpoint_id,
        owner: environment_owner(access.environment.id),
        target: EndpointTarget::Product,
        hosts: Vec::new(),
        path_prefix,
        protocol: EndpointProtocol::Http,
        server_requires_tls: false,
        access: policies,
    }
}

/// A sandbox preview's endpoint (its id is the sandbox id; previews are
/// compiled per lookup, never into the route snapshot).
///
/// - the IP allow-list comes first, whatever the mode: a non-empty list
///   admits only the listed networks and refuses an unresolved client;
/// - `public` is open to anyone who passes it;
/// - `password` is the password wall, and without a hash nobody passes (the
///   database refuses that row; this is the belt to its braces);
/// - `temps_login` asks for a Temps login by a user who may read the
///   sandbox.
pub fn sandbox_endpoint(sandbox: &sandboxes::Model) -> Endpoint {
    let mut access = Vec::with_capacity(2);
    if !sandbox.preview_allowed_cidrs.is_empty() {
        access.push(AccessPolicy::IpRules {
            rules: sandbox
                .preview_allowed_cidrs
                .iter()
                .map(|cidr| IpRule {
                    cidr: cidr.clone(),
                    allow: true,
                })
                .collect(),
            unresolved: UnresolvedClient::Deny,
        });
    }
    access.push(match sandbox.preview_access {
        SandboxPreviewAccess::Public => AccessPolicy::Public,
        SandboxPreviewAccess::Password if sandbox.preview_password_hash.is_some() => {
            AccessPolicy::Password {
                password_ref: i64::from(sandbox.id),
            }
        }
        SandboxPreviewAccess::Password => deny_everyone(),
        SandboxPreviewAccess::TempsLogin => AccessPolicy::TempsLogin {
            permission: SANDBOX_PREVIEW_PERMISSION.to_string(),
        },
    });
    Endpoint {
        id: i64::from(sandbox.id),
        owner: sandbox_owner(sandbox.id),
        target: EndpointTarget::Product,
        hosts: Vec::new(),
        path_prefix: None,
        protocol: EndpointProtocol::Http,
        server_requires_tls: false,
        access,
    }
}

/// The compiled ingress half of a route snapshot.
#[derive(Debug, Default)]
pub struct IngressSnapshot {
    routes: CompiledRoutes,
    /// `password_ref` (the environment id) → argon2 hash.
    passwords: HashMap<i64, Box<str>>,
    revocations: Revocations,
    /// Non-legacy environments whose endpoint did not compile. An enforcing
    /// proxy refuses their requests rather than guessing.
    broken: HashSet<i32>,
    /// Whether this snapshot was built from a completed load. The default
    /// (before the first load succeeds) is not: it knows no environment's
    /// policy, so an enforcing proxy must refuse rather than read its
    /// emptiness as "no restrictions".
    loaded: bool,
}

impl IngressSnapshot {
    /// Compiles `endpoints`. One that does not compile is left out and its
    /// environment recorded as broken; the rest still publish.
    pub fn build(
        endpoints: Vec<Endpoint>,
        passwords: HashMap<i64, Box<str>>,
        revocations: Revocations,
    ) -> Self {
        let mut broken = HashSet::new();
        let mut good = Vec::with_capacity(endpoints.len());
        for endpoint in endpoints {
            match CompiledEndpoint::compile(&endpoint) {
                Ok(_) => good.push(endpoint),
                Err(e) => {
                    error!(
                        endpoint_id = endpoint.id,
                        owner = %endpoint.owner,
                        error = %e,
                        "Ingress endpoint does not compile; its requests are refused while it enforces"
                    );
                    if endpoint.owner.kind == ProductKind::Environment {
                        if let Ok(id) = i32::try_from(endpoint.owner.id) {
                            broken.insert(id);
                        }
                    }
                }
            }
        }
        let routes = match CompiledRoutes::compile(&good) {
            Ok(routes) => routes,
            Err(e) => {
                error!(error = %e, "Ingress endpoints did not compile together; refusing every enforcing environment");
                for endpoint in &good {
                    if let Ok(id) = i32::try_from(endpoint.owner.id) {
                        broken.insert(id);
                    }
                }
                CompiledRoutes::default()
            }
        };
        Self {
            routes,
            passwords,
            revocations,
            broken,
            loaded: true,
        }
    }

    /// Whether this snapshot came from a completed load (see `loaded`).
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The endpoint an environment's request at `path` is decided by.
    pub fn environment_endpoint(
        &self,
        environment_id: i32,
        path: &str,
    ) -> Option<&CompiledEndpoint> {
        self.routes
            .for_owner(environment_owner(environment_id), path)
    }

    pub fn is_broken(&self, environment_id: i32) -> bool {
        !self.broken.is_empty() && self.broken.contains(&environment_id)
    }

    pub fn password_hash(&self, password_ref: i64) -> Option<&str> {
        self.passwords.get(&password_ref).map(|hash| &**hash)
    }

    pub fn revocations(&self) -> &Revocations {
        &self.revocations
    }

    pub fn endpoints(&self) -> &[CompiledEndpoint] {
        self.routes.endpoints()
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// Grants that must no longer admit anyone, among those not yet expired:
/// the ones revoked explicitly, and every grant of a deleted user. The
/// writes that remove access revoke explicitly; the deleted-user arm also
/// covers a deletion that bypassed them (a manual `UPDATE users`), as of
/// the next reload.
pub async fn load_revocations(db: &DatabaseConnection) -> Result<Revocations, IngressLoadError> {
    let ids: Vec<i64> = ingress_login_grants::Entity::find()
        .select_only()
        .column(ingress_login_grants::Column::Id)
        .join(
            JoinType::InnerJoin,
            ingress_login_grants::Relation::User.def(),
        )
        .filter(ingress_login_grants::Column::ExpiresAt.gt(chrono::Utc::now()))
        .filter(
            Condition::any()
                .add(ingress_login_grants::Column::RevokedAt.is_not_null())
                .add(users::Column::DeletedAt.is_not_null()),
        )
        .into_tuple()
        .all(db)
        .await
        .map_err(db_err("revoked login grants"))?;
    Ok(Revocations::new(ids))
}

/// Loads and compiles the endpoints of every non-legacy environment.
pub async fn load_ingress_snapshot(
    db: &DatabaseConnection,
    instance_rate_limiting: &RateLimitSettings,
) -> Result<IngressSnapshot, IngressLoadError> {
    let revocations = load_revocations(db).await?;
    let environments: Vec<(environments::Model, Option<projects::Model>)> =
        environments::Entity::find()
            .filter(environments::Column::IngressMode.ne(IngressMode::Legacy))
            .filter(environments::Column::DeletedAt.is_null())
            .find_also_related(projects::Entity)
            .all(db)
            .await
            .map_err(db_err("environments"))?;
    if environments.is_empty() {
        return Ok(IngressSnapshot::build(
            Vec::new(),
            HashMap::new(),
            revocations,
        ));
    }

    let project_ids: Vec<i32> = {
        let mut ids: Vec<i32> = environments.iter().map(|(env, _)| env.project_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let environment_ids: Vec<i32> = environments.iter().map(|(env, _)| env.id).collect();

    let ip_rules = project_ip_access_rules::Entity::find()
        .filter(project_ip_access_rules::Column::ProjectId.is_in(project_ids))
        .all(db)
        .await
        .map_err(db_err("project IP access rules"))?;
    let mut rules_by_project: HashMap<i32, Vec<project_ip_access_rules::Model>> = HashMap::new();
    for row in ip_rules {
        rules_by_project
            .entry(row.project_id)
            .or_default()
            .push(row);
    }

    let endpoint_rows = ingress_endpoints::Entity::find()
        .filter(ingress_endpoints::Column::EnvironmentId.is_in(environment_ids))
        .all(db)
        .await
        .map_err(db_err("endpoints"))?;
    let endpoint_ids: Vec<i64> = endpoint_rows.iter().map(|row| row.id).collect();
    let mut stored: HashMap<i64, Vec<AccessPolicy>> = HashMap::new();
    if !endpoint_ids.is_empty() {
        for row in ingress_access_policies::Entity::find()
            .filter(ingress_access_policies::Column::EndpointId.is_in(endpoint_ids))
            .order_by_asc(ingress_access_policies::Column::EndpointId)
            .order_by_asc(ingress_access_policies::Column::Position)
            .all(db)
            .await
            .map_err(db_err("access policies"))?
        {
            stored
                .entry(row.endpoint_id)
                .or_default()
                .push(row.policy.0);
        }
    }
    let mut rows_by_environment: HashMap<i32, Vec<ingress_endpoints::Model>> = HashMap::new();
    for row in endpoint_rows {
        if let Some(env) = row.environment_id {
            rows_by_environment.entry(env).or_default().push(row);
        }
    }

    let mut endpoints = Vec::new();
    let mut passwords = HashMap::new();
    let mut unsupported = Vec::new();
    let no_rules = Vec::new();
    for (environment, project) in &environments {
        let Some(project) = project else {
            warn!(
                environment_id = environment.id,
                project_id = environment.project_id,
                "Environment has no project; no ingress endpoint compiled"
            );
            continue;
        };
        let access = EnvironmentAccess {
            environment,
            project,
            instance_rate_limiting,
            ip_rules: rules_by_project
                .get(&environment.project_id)
                .unwrap_or(&no_rules),
        };
        if let Some(hash) = password_hash(environment) {
            passwords.insert(i64::from(environment.id), Box::<str>::from(hash));
        }
        match rows_by_environment.remove(&environment.id) {
            Some(rows) => {
                for row in rows {
                    let policies = stored.remove(&row.id).unwrap_or_default();
                    if let Some(policy) = policies.iter().find(|p| !is_storable(p)) {
                        error!(
                            endpoint_id = row.id,
                            environment_id = environment.id,
                            kind = policy.kind().as_str(),
                            "Stored access policy kind is not enforced in this phase; the environment's requests are refused while it enforces"
                        );
                        unsupported.push(environment.id);
                        continue;
                    }
                    endpoints.push(environment_endpoint(
                        row.id,
                        &access,
                        row.path_prefix.clone(),
                        policies,
                    ));
                }
            }
            // The trigger creates the row; a missing one (a manual delete)
            // still gets the derived endpoint, under an id no row can have.
            None => endpoints.push(environment_endpoint(
                -i64::from(environment.id),
                &access,
                None,
                Vec::new(),
            )),
        }
    }
    let mut snapshot = IngressSnapshot::build(endpoints, passwords, revocations);
    snapshot.broken.extend(unsupported);
    Ok(snapshot)
}

/// Whether a stored (explicit) policy is one this phase enforces. The
/// derived policies already carry the password wall, rate limits and
/// captcha, keyed the way the legacy chain keys them; a second stored copy
/// would share their counters and grants ambiguously, so stored rows are
/// limited to the policies that add access control on top: an IP list and a
/// Temps login. The service refuses the rest on write; a row written by
/// hand refuses its environment rather than being silently skipped.
pub fn is_storable(policy: &AccessPolicy) -> bool {
    matches!(
        policy,
        AccessPolicy::IpRules { .. } | AccessPolicy::TempsLogin { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::deployment_config::{
        DeploymentConfig, PasswordProtectionConfig, RateLimitConfig, SecurityConfig,
    };
    use temps_entities::project_ip_access_rules::{IpAccessCidrEntry, IpAccessCidrs};

    pub(crate) fn environment(id: i32, project_id: i32) -> environments::Model {
        let now = chrono::Utc::now();
        environments::Model {
            id,
            name: "production".into(),
            slug: "production".into(),
            subdomain: format!("app-{id}"),
            last_deployment: None,
            host: String::new(),
            upstreams: Default::default(),
            created_at: now,
            updated_at: now,
            project_id,
            current_deployment_id: None,
            branch: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            force_https: None,
            last_activity_at: None,
            ingress_mode: IngressMode::Enforce,
            workload_mode: Default::default(),
        }
    }

    fn rule(
        project_id: i32,
        environment_id: Option<i32>,
        mode: &str,
        cidrs: &[&str],
    ) -> project_ip_access_rules::Model {
        let now = chrono::Utc::now();
        project_ip_access_rules::Model {
            id: 1,
            project_id,
            environment_id,
            mode: mode.into(),
            cidrs: IpAccessCidrs(
                cidrs
                    .iter()
                    .map(|cidr| IpAccessCidrEntry {
                        cidr: cidr.to_string(),
                        description: None,
                    })
                    .collect(),
            ),
            updated_by: None,
            revision: 1,
            created_at: now,
            updated_at: now,
        }
    }

    fn project(id: i32) -> projects::Model {
        let now = chrono::Utc::now();
        projects::Model {
            id,
            name: format!("project-{id}"),
            repo_name: format!("project-{id}"),
            repo_owner: "test".to_string(),
            directory: String::new(),
            pull_only_root_directory: false,
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: format!("project-{id}"),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            error_source_context_enabled: false,
            vulnerability_scanning_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: Default::default(),
            project_type: temps_entities::types::ProjectType::Server,
            allow_alternate_sources: None,
            hosting: Default::default(),
            template_slug: None,
            service_template: None,
            github_webhook_id: None,
            github_webhook_signing_token: None,
            serves_own_temps_api: false,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: false,
            ai_api_traffic_summary_enabled: None,
            image_retention_hours: None,
            cloud_telemetry_fidelity: Default::default(),
            cloud_telemetry_attribute_allowlist: Vec::new(),
            cloud_telemetry_write_mode: Default::default(),
            cloud_analytics_write_mode: Default::default(),
            instantiation_token: None,
            log_ingest_lines_per_sec: None,
            log_ingest_kib_per_sec: None,
        }
    }

    fn sandbox(id: i32) -> sandboxes::Model {
        let now = chrono::Utc::now();
        sandboxes::Model {
            id,
            public_id: format!("sbx_{id:012}"),
            user_id: Some(1),
            agent_run_id: None,
            name: format!("sandbox-{id}"),
            status: "running".into(),
            lifecycle: "ephemeral".into(),
            project_id: None,
            source_repo_url: None,
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 600,
            metadata: None,
            backend: None,
            node_id: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: SandboxPreviewAccess::TempsLogin,
            preview_allowed_cidrs: Vec::new(),
            egress_policy: Default::default(),
        }
    }

    fn derive(
        env: &environments::Model,
        project: &projects::Model,
        instance: &RateLimitSettings,
        rules: &[project_ip_access_rules::Model],
    ) -> Vec<AccessPolicy> {
        derived_policies(&EnvironmentAccess {
            environment: env,
            project,
            instance_rate_limiting: instance,
            ip_rules: rules,
        })
    }

    #[test]
    fn an_environment_with_no_settings_is_public() {
        let env = environment(7, 3);
        assert!(derive(&env, &project(3), &RateLimitSettings::default(), &[]).is_empty());
    }

    #[test]
    fn policies_follow_the_legacy_order() {
        let mut env = environment(7, 3);
        env.attack_mode = Some(true);
        env.deployment_config = Some(DeploymentConfig {
            security: Some(SecurityConfig {
                enabled: Some(true),
                rate_limiting: Some(RateLimitConfig {
                    max_requests_per_minute: Some(10),
                    max_requests_per_hour: None,
                    whitelist_ips: vec!["198.51.100.7".into()],
                    blacklist_ips: vec!["203.0.113.0/24".into(), "garbage".into()],
                }),
                password_protection: Some(PasswordProtectionConfig {
                    enabled: true,
                    password_hash: "$argon2id$hash".into(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        let instance = RateLimitSettings {
            enabled: true,
            max_requests_per_minute: 0,
            max_requests_per_hour: 500,
            whitelist_ips: vec![],
            blacklist_ips: vec!["192.0.2.9".into()],
        };
        let rules = [rule(3, None, "allow", &["10.0.0.0/8"])];
        let policies = derive(&env, &project(3), &instance, &rules);
        let kinds: Vec<&str> = policies.iter().map(|p| p.name()).collect();
        assert_eq!(
            kinds,
            ["ip_rules", "ip_rules", "rate_limit", "captcha", "password"]
        );
        assert_eq!(
            policies[1],
            AccessPolicy::IpRules {
                rules: vec![
                    IpRule {
                        cidr: "192.0.2.9/32".into(),
                        allow: false
                    },
                    IpRule {
                        cidr: "203.0.113.0/24".into(),
                        allow: false
                    },
                ],
                unresolved: UnresolvedClient::Admit,
            }
        );
        assert_eq!(
            policies[2],
            AccessPolicy::RateLimit {
                limits: vec![
                    RateLimit {
                        requests: 10,
                        per_secs: 60
                    },
                    RateLimit {
                        requests: 500,
                        per_secs: 3600
                    },
                ],
                exempt: vec!["198.51.100.7/32".into()],
                scope: RateLimitScope::Endpoint,
            }
        );
        assert_eq!(policies[4], AccessPolicy::Password { password_ref: 7 });
    }

    #[test]
    fn the_ip_gate_reads_entries_as_the_proxy_does() {
        let env = environment(7, 3);
        let p = project(3);
        let none = RateLimitSettings::default();
        // An environment override wins, even a disabled one.
        let rules = [
            rule(3, None, "allow", &["10.0.0.0/8"]),
            rule(3, Some(7), "disabled", &[]),
        ];
        assert!(derive(&env, &p, &none, &rules).is_empty());
        // Host bits make an allow entry unreadable; an allow list left empty
        // refuses everyone.
        let rules = [rule(3, None, "allow", &["10.0.0.5/8"])];
        assert_eq!(derive(&env, &p, &none, &rules), vec![deny_everyone()]);
        // One unreadable deny entry refuses everyone.
        let rules = [rule(3, None, "deny", &["192.0.2.0/24", "nope"])];
        assert_eq!(derive(&env, &p, &none, &rules), vec![deny_everyone()]);
        // An empty deny list enforces nothing.
        let rules = [rule(3, None, "deny", &[])];
        assert!(derive(&env, &p, &none, &rules).is_empty());
        // Mapped IPv6 folds into IPv4; an unknown mode refuses everyone.
        let rules = [rule(3, None, "allow", &["::ffff:10.0.0.0/104"])];
        assert_eq!(
            derive(&env, &p, &none, &rules),
            vec![AccessPolicy::IpRules {
                rules: vec![IpRule {
                    cidr: "10.0.0.0/8".into(),
                    allow: true
                }],
                unresolved: UnresolvedClient::Deny,
            }]
        );
        let rules = [rule(3, None, "sometimes", &[])];
        assert_eq!(derive(&env, &p, &none, &rules), vec![deny_everyone()]);
        // Another project's rows do not apply.
        let rules = [rule(4, None, "deny", &["192.0.2.0/24"])];
        assert!(derive(&env, &p, &none, &rules).is_empty());
    }

    #[test]
    fn sandbox_endpoints_follow_the_access_mode_after_the_allow_list() {
        let mut sandbox = sandbox(5);
        assert_eq!(SANDBOX_PREVIEW_PERMISSION, "sandboxes:read");
        assert_eq!(
            sandbox_endpoint(&sandbox).access,
            vec![AccessPolicy::TempsLogin {
                permission: SANDBOX_PREVIEW_PERMISSION.into()
            }]
        );
        sandbox.preview_access = SandboxPreviewAccess::Public;
        assert_eq!(
            sandbox_endpoint(&sandbox).access,
            vec![AccessPolicy::Public]
        );
        // A password only protects in `password` mode; without a hash the
        // mode admits nobody.
        sandbox.preview_access = SandboxPreviewAccess::Password;
        assert_eq!(sandbox_endpoint(&sandbox).access, vec![deny_everyone()]);
        sandbox.preview_password_hash = Some("$argon2id$hash".into());
        assert_eq!(
            sandbox_endpoint(&sandbox).access,
            vec![AccessPolicy::Password { password_ref: 5 }]
        );
        // The allow-list is evaluated first and fails closed on an unknown
        // client.
        sandbox.preview_allowed_cidrs = vec!["192.0.2.0/24".into(), "2001:db8::/32".into()];
        let access = sandbox_endpoint(&sandbox).access;
        assert_eq!(
            access[0],
            AccessPolicy::IpRules {
                rules: vec![
                    IpRule {
                        cidr: "192.0.2.0/24".into(),
                        allow: true
                    },
                    IpRule {
                        cidr: "2001:db8::/32".into(),
                        allow: true
                    },
                ],
                unresolved: UnresolvedClient::Deny,
            }
        );
        assert_eq!(access[1], AccessPolicy::Password { password_ref: 5 });
    }

    #[test]
    fn a_broken_endpoint_is_left_out_and_recorded() {
        let env = environment(7, 3);
        let p = project(3);
        let none = RateLimitSettings::default();
        let access = EnvironmentAccess {
            environment: &env,
            project: &p,
            instance_rate_limiting: &none,
            ip_rules: &[],
        };
        let good = environment_endpoint(1, &access, None, Vec::new());
        let mut other = environment(8, 3);
        other.id = 8;
        let bad_access = EnvironmentAccess {
            environment: &other,
            ..access
        };
        let bad = environment_endpoint(
            2,
            &bad_access,
            None,
            vec![AccessPolicy::Geo {
                allow: vec!["not-a-country".into()],
                deny: vec![],
            }],
        );
        let snapshot =
            IngressSnapshot::build(vec![good, bad], HashMap::new(), Revocations::default());
        assert!(snapshot.environment_endpoint(7, "/").is_some());
        assert!(snapshot.environment_endpoint(8, "/").is_none());
        assert!(snapshot.is_broken(8));
        assert!(!snapshot.is_broken(7));
    }

    #[tokio::test]
    async fn revocations_cover_revoked_grants_and_grants_of_deleted_users() {
        use sea_orm::{ActiveModelTrait, Set};
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error)
                if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none()
                    && temps_database::test_utils::is_container_runtime_unavailable(
                        &error.to_string(),
                    ) =>
            {
                println!("Docker not available, skipping: {error}");
                return;
            }
            Err(error) => panic!("test database failed to start: {error}"),
        };
        let db = test_db.db.as_ref();
        let now = chrono::Utc::now();
        let user = |email: &str, deleted: bool| users::ActiveModel {
            name: Set(email.to_string()),
            email: Set(email.to_string()),
            deleted_at: Set(deleted.then_some(now)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let active = user("active@example.test", false)
            .insert(db)
            .await
            .expect("insert active user")
            .id;
        let deleted = user("deleted@example.test", true)
            .insert(db)
            .await
            .expect("insert deleted user")
            .id;
        let grant = |user_id: i32, revoked: bool, expires_in_hours: i64| {
            ingress_login_grants::ActiveModel {
                user_id: Set(user_id),
                owner_kind: Set("environment".to_string()),
                owner_id: Set(1),
                host: Set("app.example.test".to_string()),
                permission: Set(ENVIRONMENT_LOGIN_PERMISSION.to_string()),
                expires_at: Set(now + chrono::Duration::hours(expires_in_hours)),
                revoked_at: Set(revoked.then_some(now)),
                created_at: Set(now),
                ..Default::default()
            }
        };
        let live = grant(active, false, 12).insert(db).await.expect("grant").id;
        let revoked = grant(active, true, 12).insert(db).await.expect("grant").id;
        let of_deleted = grant(deleted, false, 12)
            .insert(db)
            .await
            .expect("grant")
            .id;
        let expired_of_deleted = grant(deleted, false, -1)
            .insert(db)
            .await
            .expect("grant")
            .id;

        let revocations = load_revocations(db).await.expect("load revocations");
        assert!(!revocations.contains(live));
        assert!(revocations.contains(revoked));
        assert!(revocations.contains(of_deleted));
        // Expired grants admit nobody; the snapshot need not carry them.
        assert!(!revocations.contains(expired_of_deleted));
        assert_eq!(revocations.len(), 2);
    }

    #[test]
    fn only_a_built_snapshot_counts_as_loaded() {
        assert!(!IngressSnapshot::default().is_loaded());
        let built = IngressSnapshot::build(Vec::new(), HashMap::new(), Revocations::default());
        assert!(built.is_loaded());
        assert!(built.is_empty());
    }

    #[test]
    fn only_ip_lists_and_temps_logins_are_storable() {
        assert!(is_storable(&AccessPolicy::TempsLogin {
            permission: ENVIRONMENT_LOGIN_PERMISSION.into(),
        }));
        assert!(is_storable(&AccessPolicy::IpRules {
            rules: vec![],
            unresolved: UnresolvedClient::Deny,
        }));
        assert!(!is_storable(&AccessPolicy::Password { password_ref: 1 }));
        assert!(!is_storable(&AccessPolicy::Captcha));
        assert!(!is_storable(&AccessPolicy::Public));
    }
}
