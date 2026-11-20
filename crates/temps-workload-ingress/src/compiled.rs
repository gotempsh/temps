// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The compiled route table and access evaluator.
//!
//! Built on the control plane from admitted endpoints, then shared
//! read-only (behind the proxy's `ArcSwap`). [`CompiledRoutes::route`],
//! [`CompiledRoutes::for_owner`] and [`CompiledEndpoint::evaluate`] take no
//! locks, do no I/O and allocate nothing: hosts are lowercased into a stack
//! buffer, policies are pre-parsed, and everything a decision needs is
//! either in the table or asked of the caller through [`RequestContext`]
//! (grant cookies verified against keys in the same snapshot, the atomic
//! rate-limit counters).
//!
//! **Claims.** Several endpoints may share a host when one product owns it
//! and they differ by path prefix (ADR-050 path routes). A host claimed by
//! two products goes to the lowest claim id; the other endpoints are left
//! out and reported as [`ClaimConflict`]s, never silently merged.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use crate::admission::AdmissionError;
use crate::endpoint::{
    AccessPolicy, Endpoint, EndpointProtocol, EndpointTarget, PolicyKind, ProductRef, RateLimit,
    RateLimitScope, UnresolvedClient,
};

/// An address block, pre-parsed. IPv4 blocks never match IPv6 addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    bits: u128,
    mask: u128,
    v4: bool,
}

impl Cidr {
    pub fn matches(&self, ip: IpAddr) -> bool {
        let (bits, v4) = match ip {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), true),
            IpAddr::V6(v6) => (u128::from(v6), false),
        };
        v4 == self.v4 && bits & self.mask == self.bits
    }
}

/// Parses `<address>/<prefix>` with no host bits set.
pub fn parse_cidr(value: &str) -> Option<Cidr> {
    let (addr, prefix) = value.split_once('/')?;
    let addr: IpAddr = addr.parse().ok()?;
    let prefix: u32 = prefix.parse().ok()?;
    let (bits, mask) = mask_for(addr, prefix)?;
    (bits & mask == bits).then_some(Cidr {
        bits,
        mask,
        v4: addr.is_ipv4(),
    })
}

fn mask_for(addr: IpAddr, prefix: u32) -> Option<(u128, u128)> {
    let (bits, width, v4) = match addr {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32, true),
        IpAddr::V6(v6) => (u128::from(v6), 128, false),
    };
    if prefix > width {
        return None;
    }
    let ones = if prefix == 0 {
        0
    } else {
        u128::MAX << (width - prefix)
    };
    let mask = if v4 {
        ones & u128::from(u32::MAX)
    } else {
        ones
    };
    Some((bits, mask))
}

/// Canonicalises a hand-written address list entry into a strict
/// `<network>/<prefix>`, the form [`parse_cidr`] accepts:
///
/// - a bare address becomes a single-address block; an IPv4-mapped IPv6
///   address is read as the IPv4 address it maps;
/// - `<address>/<prefix>` with host bits set keeps its network (the bits
///   under the mask), the way `ipnetwork`-style containment reads it.
///
/// `None` for anything else. Used to translate address lists stored by
/// earlier features, which accepted both forms, into typed policies.
pub fn canonical_cidr(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    let (addr, prefix) = match entry.split_once('/') {
        Some((addr, prefix)) => {
            let addr: IpAddr = addr.trim().parse().ok()?;
            (addr, prefix.trim().parse::<u32>().ok()?)
        }
        None => {
            let addr: IpAddr = entry.parse().ok()?;
            let addr = match addr {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(addr),
                v4 => v4,
            };
            (addr, if addr.is_ipv4() { 32 } else { 128 })
        }
    };
    let (bits, mask) = mask_for(addr, prefix)?;
    let network = bits & mask;
    let network = match addr {
        IpAddr::V4(_) => IpAddr::V4(std::net::Ipv4Addr::from(network as u32)),
        IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::from(network)),
    };
    Some(format!("{network}/{prefix}"))
}

/// A rate limit, pre-parsed. The caller owns the counters; this tells it
/// which windows to apply and against which budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRateLimit {
    limits: Box<[RateLimit]>,
    exempt: Box<[Cidr]>,
    scope: RateLimitScope,
}

impl CompiledRateLimit {
    pub fn limits(&self) -> &[RateLimit] {
        &self.limits
    }

    pub fn scope(&self) -> RateLimitScope {
        self.scope
    }

    /// The budget of the window `per_secs` long, if this limit has one.
    pub fn window(&self, per_secs: u32) -> Option<u32> {
        self.limits
            .iter()
            .find(|limit| limit.per_secs == per_secs)
            .map(|limit| limit.requests)
    }

    fn exempts(&self, ip: IpAddr) -> bool {
        self.exempt.iter().any(|cidr| cidr.matches(ip))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Policy {
    Public,
    Password {
        password_ref: i64,
    },
    TempsLogin {
        permission: Box<str>,
    },
    SignedLink,
    IpRules {
        rules: Box<[(Cidr, bool)]>,
        any_allow: bool,
        unresolved: UnresolvedClient,
    },
    RateLimit(CompiledRateLimit),
    Geo {
        allow: Box<[[u8; 2]]>,
        deny: Box<[[u8; 2]]>,
    },
    Captcha,
}

fn country(code: &str) -> Option<[u8; 2]> {
    match code.as_bytes() {
        [a, b] if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
            Some([a.to_ascii_uppercase(), b.to_ascii_uppercase()])
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledEndpoint {
    pub id: i64,
    pub owner: ProductRef,
    pub target: EndpointTarget,
    pub protocol: EndpointProtocol,
    prefix: Box<str>,
    policies: Box<[Policy]>,
    kinds: u16,
}

impl CompiledEndpoint {
    /// Compiles one endpoint's policies. Fails on what admission refuses:
    /// an unparsable address block or country code.
    pub fn compile(e: &Endpoint) -> Result<Self, AdmissionError> {
        let mut policies = Vec::with_capacity(e.access.len());
        let mut kinds = 0u16;
        for policy in &e.access {
            kinds |= policy.kind().bit();
            policies.push(match policy {
                AccessPolicy::Public => Policy::Public,
                AccessPolicy::Password { password_ref } => Policy::Password {
                    password_ref: *password_ref,
                },
                AccessPolicy::TempsLogin { permission } => Policy::TempsLogin {
                    permission: permission.as_str().into(),
                },
                AccessPolicy::SignedLink => Policy::SignedLink,
                AccessPolicy::IpRules { rules, unresolved } => {
                    let mut parsed = Vec::with_capacity(rules.len());
                    for rule in rules {
                        let cidr = parse_cidr(&rule.cidr).ok_or_else(|| {
                            AdmissionError::InvalidIpRule {
                                endpoint_id: e.id,
                                cidr: rule.cidr.clone(),
                            }
                        })?;
                        parsed.push((cidr, rule.allow));
                    }
                    Policy::IpRules {
                        any_allow: rules.iter().any(|r| r.allow),
                        rules: parsed.into(),
                        unresolved: *unresolved,
                    }
                }
                AccessPolicy::RateLimit {
                    limits,
                    exempt,
                    scope,
                } => {
                    let mut parsed = Vec::with_capacity(exempt.len());
                    for cidr in exempt {
                        parsed.push(parse_cidr(cidr).ok_or_else(|| {
                            AdmissionError::InvalidIpRule {
                                endpoint_id: e.id,
                                cidr: cidr.clone(),
                            }
                        })?);
                    }
                    Policy::RateLimit(CompiledRateLimit {
                        limits: limits.as_slice().into(),
                        exempt: parsed.into(),
                        scope: *scope,
                    })
                }
                AccessPolicy::Geo { allow, deny } => {
                    let parse = |codes: &[String]| -> Result<Box<[[u8; 2]]>, AdmissionError> {
                        codes
                            .iter()
                            .map(|c| {
                                country(c).ok_or_else(|| AdmissionError::InvalidCountry {
                                    endpoint_id: e.id,
                                    code: c.clone(),
                                })
                            })
                            .collect()
                    };
                    Policy::Geo {
                        allow: parse(allow)?,
                        deny: parse(deny)?,
                    }
                }
                AccessPolicy::Captcha => Policy::Captcha,
            });
        }
        Ok(CompiledEndpoint {
            id: e.id,
            owner: e.owner,
            target: e.target.clone(),
            protocol: e.protocol,
            prefix: e.path_prefix.as_deref().unwrap_or("/").into(),
            policies: policies.into(),
            kinds,
        })
    }

    pub fn path_prefix(&self) -> &str {
        &self.prefix
    }

    /// Whether any of the endpoint's policies is of `kind`. Lets a caller
    /// prepare a fact (an asynchronous cache read) only when a policy will
    /// ask for it.
    pub fn has(&self, kind: PolicyKind) -> bool {
        self.kinds & kind.bit() != 0
    }

    /// The password the endpoint's wall checks, if it has one.
    pub fn password_ref(&self) -> Option<i64> {
        self.policies.iter().find_map(|p| match p {
            Policy::Password { password_ref } => Some(*password_ref),
            _ => None,
        })
    }

    /// The permission the endpoint's `TempsLogin` policy requires.
    pub fn login_permission(&self) -> Option<&str> {
        self.policies.iter().find_map(|p| match p {
            Policy::TempsLogin { permission } => Some(&**permission),
            _ => None,
        })
    }

    fn matches_path(&self, path: &str) -> bool {
        let prefix = &*self.prefix;
        if prefix == "/" {
            return true;
        }
        match path.strip_prefix(prefix) {
            Some(rest) => rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'),
            None => false,
        }
    }

    /// Runs the policies in order; the first that does not pass decides.
    pub fn evaluate(&self, ctx: &impl RequestContext) -> Decision<'_> {
        self.evaluate_from(0, ctx).0
    }

    /// Runs the policies from position `start` on and returns the decision
    /// with the position of the policy that made it (the policy count for
    /// `Allow`).
    ///
    /// A caller whose fact for one policy needs an asynchronous read (a
    /// captcha completion behind a cache) answers `false` for it, and when
    /// that policy is the one that decided, reads the fact and resumes
    /// after it, so the policies before it (a rate limit that counts) run
    /// exactly once.
    pub fn evaluate_from(&self, start: usize, ctx: &impl RequestContext) -> (Decision<'_>, usize) {
        for (at, policy) in self.policies.iter().enumerate().skip(start) {
            if let Some(decision) = self.check(policy, ctx) {
                return (decision, at);
            }
        }
        (Decision::Allow, self.policies.len())
    }

    /// One policy: `None` when it passes.
    fn check<'a>(&'a self, policy: &'a Policy, ctx: &impl RequestContext) -> Option<Decision<'a>> {
        match policy {
            Policy::Public => None,
            Policy::Password { password_ref } => (!ctx.has_password_grant(self, *password_ref))
                .then_some(Decision::Challenge(Challenge::Password {
                    password_ref: *password_ref,
                })),
            Policy::TempsLogin { permission } => (!ctx.has_login_grant(self, permission))
                .then_some(Decision::Challenge(Challenge::TempsLogin { permission })),
            Policy::SignedLink => (!ctx.has_signed_link(self))
                .then_some(Decision::Deny(DenyReason::SignedLinkRequired)),
            Policy::IpRules {
                rules,
                any_allow,
                unresolved,
            } => {
                let Some(ip) = ctx.client_ip() else {
                    return match unresolved {
                        UnresolvedClient::Deny => Some(Decision::Deny(DenyReason::IpNotAllowed)),
                        UnresolvedClient::Admit => None,
                    };
                };
                match rules.iter().find(|(cidr, _)| cidr.matches(ip)) {
                    Some((_, true)) => None,
                    Some((_, false)) => Some(Decision::Deny(DenyReason::IpNotAllowed)),
                    None if *any_allow => Some(Decision::Deny(DenyReason::IpNotAllowed)),
                    None => None,
                }
            }
            Policy::RateLimit(limit) => {
                // An unknown client is not counted; an exempt one never is.
                let ip = ctx.client_ip()?;
                (!limit.exempts(ip) && !ctx.within_rate_limit(self, limit))
                    .then_some(Decision::TooManyRequests)
            }
            Policy::Geo { allow, deny } => {
                let code = ctx
                    .country()
                    .map(|[a, b]| [a.to_ascii_uppercase(), b.to_ascii_uppercase()]);
                let denied = code.is_some_and(|c| deny.contains(&c));
                let not_allowed = !allow.is_empty() && !code.is_some_and(|c| allow.contains(&c));
                (denied || not_allowed).then_some(Decision::Deny(DenyReason::CountryNotAllowed))
            }
            Policy::Captcha => {
                (!ctx.captcha_passed(self)).then_some(Decision::Challenge(Challenge::Captcha))
            }
        }
    }
}

/// Two products claimed one host, or one product claimed the same host and
/// path twice. The loser is not routed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimConflict {
    pub host: String,
    pub path_prefix: String,
    pub winner_endpoint: i64,
    pub loser_endpoint: i64,
}

/// What the caller knows about the request, beyond host and path. Every
/// method must answer from memory: the snapshot's keys, the atomic
/// counters, the parsed cookies.
pub trait RequestContext {
    /// `None` when the client's address could not be resolved.
    fn client_ip(&self) -> Option<IpAddr>;
    /// ISO 3166-1 alpha-2, any case.
    fn country(&self) -> Option<[u8; 2]>;
    fn has_password_grant(&self, endpoint: &CompiledEndpoint, password_ref: i64) -> bool;
    fn has_login_grant(&self, endpoint: &CompiledEndpoint, permission: &str) -> bool;
    fn has_signed_link(&self, endpoint: &CompiledEndpoint) -> bool;
    fn captcha_passed(&self, endpoint: &CompiledEndpoint) -> bool;
    /// Counts the request against `limit` and says whether it fits. Only
    /// asked for a resolved, non-exempt client.
    fn within_rate_limit(&self, endpoint: &CompiledEndpoint, limit: &CompiledRateLimit) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Challenge<'a> {
    Password {
        password_ref: i64,
    },
    /// Redirect to the console, which signs a grant for this host.
    TempsLogin {
        permission: &'a str,
    },
    Captcha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    IpNotAllowed,
    CountryNotAllowed,
    SignedLinkRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision<'a> {
    Allow,
    Challenge(Challenge<'a>),
    Deny(DenyReason),
    TooManyRequests,
}

impl Decision<'_> {
    /// A stable label for logs and metrics.
    pub fn label(&self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Challenge(Challenge::Password { .. }) => "challenge_password",
            Decision::Challenge(Challenge::TempsLogin { .. }) => "challenge_temps_login",
            Decision::Challenge(Challenge::Captcha) => "challenge_captcha",
            Decision::Deny(DenyReason::IpNotAllowed) => "deny_ip",
            Decision::Deny(DenyReason::CountryNotAllowed) => "deny_country",
            Decision::Deny(DenyReason::SignedLinkRequired) => "deny_signed_link",
            Decision::TooManyRequests => "too_many_requests",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompiledRoutes {
    endpoints: Box<[CompiledEndpoint]>,
    /// Host → endpoint indices, longest prefix first.
    exact: HashMap<Box<str>, Box<[u32]>>,
    /// Parent domain of a `*.` claim → endpoint indices.
    wildcard: HashMap<Box<str>, Box<[u32]>>,
    /// Owner → endpoint indices, longest prefix first.
    owners: HashMap<ProductRef, Box<[u32]>>,
    conflicts: Vec<ClaimConflict>,
}

/// The longest hostname, and so the stack buffer for lowercasing one.
const MAX_HOST: usize = 253;

impl CompiledRoutes {
    /// Compiles admitted endpoints (see [`crate::admit`]).
    pub fn compile(endpoints: &[Endpoint]) -> Result<Self, AdmissionError> {
        let mut compiled = Vec::with_capacity(endpoints.len());
        for e in endpoints {
            compiled.push(CompiledEndpoint::compile(e)?);
        }

        // host → (claim id, endpoint index), every claim of every endpoint.
        let mut claims: BTreeMap<&str, Vec<(i64, usize)>> = BTreeMap::new();
        for (index, e) in endpoints.iter().enumerate() {
            for claim in &e.hosts {
                claims
                    .entry(claim.host.as_str())
                    .or_default()
                    .push((claim.claim_id, index));
            }
        }
        let mut exact: HashMap<Box<str>, Box<[u32]>> = HashMap::new();
        let mut wildcard: HashMap<Box<str>, Box<[u32]>> = HashMap::new();
        let mut conflicts = Vec::new();
        for (host, mut holders) in claims {
            holders.sort();
            let (_, first) = holders[0];
            let owner = endpoints[first].owner;
            // Winner's endpoints by prefix; the lowest endpoint id per prefix.
            let mut by_prefix: BTreeMap<&str, usize> = BTreeMap::new();
            for &(_, index) in &holders {
                let e = &endpoints[index];
                let prefix = e.path_prefix.as_deref().unwrap_or("/");
                let winner = by_prefix.get(prefix).copied();
                let loses_to = if e.owner != owner {
                    Some(first)
                } else {
                    match winner {
                        Some(w) if endpoints[w].id <= e.id => Some(w),
                        Some(w) => {
                            conflicts.push(ClaimConflict {
                                host: host.to_string(),
                                path_prefix: prefix.to_string(),
                                winner_endpoint: e.id,
                                loser_endpoint: endpoints[w].id,
                            });
                            by_prefix.insert(prefix, index);
                            None
                        }
                        None => {
                            by_prefix.insert(prefix, index);
                            None
                        }
                    }
                };
                if let Some(w) = loses_to {
                    if w != index {
                        conflicts.push(ClaimConflict {
                            host: host.to_string(),
                            path_prefix: prefix.to_string(),
                            winner_endpoint: endpoints[w].id,
                            loser_endpoint: e.id,
                        });
                    }
                }
            }
            let mut indices: Vec<u32> = by_prefix.into_values().map(|i| i as u32).collect();
            indices.sort_by_key(|&i| std::cmp::Reverse(compiled[i as usize].prefix.len()));
            match host.strip_prefix("*.") {
                Some(parent) => wildcard.insert(parent.into(), indices.into()),
                None => exact.insert(host.into(), indices.into()),
            };
        }
        conflicts.sort_by(|a, b| (&a.host, a.loser_endpoint).cmp(&(&b.host, b.loser_endpoint)));
        conflicts.dedup();

        let mut owners: HashMap<ProductRef, Vec<u32>> = HashMap::new();
        for (index, e) in compiled.iter().enumerate() {
            owners.entry(e.owner).or_default().push(index as u32);
        }
        let owners = owners
            .into_iter()
            .map(|(owner, mut indices)| {
                indices.sort_by_key(|&i| {
                    (
                        std::cmp::Reverse(compiled[i as usize].prefix.len()),
                        compiled[i as usize].id,
                    )
                });
                (owner, indices.into_boxed_slice())
            })
            .collect();
        Ok(Self {
            endpoints: compiled.into(),
            exact,
            wildcard,
            owners,
            conflicts,
        })
    }

    pub fn conflicts(&self) -> &[ClaimConflict] {
        &self.conflicts
    }

    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    pub fn endpoints(&self) -> &[CompiledEndpoint] {
        &self.endpoints
    }

    /// The endpoint serving `host` (a `Host` header: any case, an optional
    /// port and trailing dot) and `path`. Exact hosts beat a wildcard; the
    /// longest matching prefix wins.
    pub fn route(&self, host: &str, path: &str) -> Option<&CompiledEndpoint> {
        let mut buffer = [0u8; MAX_HOST];
        let host = lowercase_host(host, &mut buffer)?;
        if let Some(found) = self.pick(self.exact.get(host).map(|b| &**b), path) {
            return Some(found);
        }
        let (_, parent) = host.split_once('.')?;
        self.pick(self.wildcard.get(parent).map(|b| &**b), path)
    }

    /// The owner's endpoint for `path`, when the product's own router has
    /// already resolved the request to that owner. The longest matching
    /// prefix wins.
    pub fn for_owner(&self, owner: ProductRef, path: &str) -> Option<&CompiledEndpoint> {
        self.pick(self.owners.get(&owner).map(|b| &**b), path)
    }

    fn pick(&self, indices: Option<&[u32]>, path: &str) -> Option<&CompiledEndpoint> {
        indices?
            .iter()
            .map(|&i| &self.endpoints[i as usize])
            .find(|e| e.matches_path(path))
    }

    /// Runs the endpoint's policies in order; the first that does not pass
    /// decides. Same as [`CompiledEndpoint::evaluate`].
    pub fn evaluate<'a>(
        &self,
        endpoint: &'a CompiledEndpoint,
        ctx: &impl RequestContext,
    ) -> Decision<'a> {
        endpoint.evaluate(ctx)
    }
}

/// `Shop.Example.TEST.:8443` → `shop.example.test`, into `buffer`. `None`
/// for IPv6 literals, non-ASCII or over-long names: nothing routes them.
fn lowercase_host<'b>(host: &str, buffer: &'b mut [u8; MAX_HOST]) -> Option<&'b str> {
    if host.starts_with('[') {
        return None;
    }
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let bytes = host.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_HOST || !host.is_ascii() {
        return None;
    }
    let out = &mut buffer[..bytes.len()];
    for (o, b) in out.iter_mut().zip(bytes) {
        *o = b.to_ascii_lowercase();
    }
    std::str::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{HostClaim, HostClaimKind, IpRule, ProductKind};

    pub(crate) struct Ctx {
        pub ip: Option<IpAddr>,
        pub country: Option<[u8; 2]>,
        pub password: bool,
        pub login: Option<&'static str>,
        pub signed: bool,
        pub captcha: bool,
        pub within_rate: bool,
    }

    impl Default for Ctx {
        fn default() -> Self {
            Self {
                ip: Some("192.0.2.10".parse().unwrap()),
                country: Some(*b"es"),
                password: false,
                login: None,
                signed: false,
                captcha: false,
                within_rate: true,
            }
        }
    }

    impl RequestContext for Ctx {
        fn client_ip(&self) -> Option<IpAddr> {
            self.ip
        }
        fn country(&self) -> Option<[u8; 2]> {
            self.country
        }
        fn has_password_grant(&self, _: &CompiledEndpoint, _: i64) -> bool {
            self.password
        }
        fn has_login_grant(&self, _: &CompiledEndpoint, permission: &str) -> bool {
            self.login == Some(permission)
        }
        fn has_signed_link(&self, _: &CompiledEndpoint) -> bool {
            self.signed
        }
        fn captcha_passed(&self, _: &CompiledEndpoint) -> bool {
            self.captcha
        }
        fn within_rate_limit(&self, _: &CompiledEndpoint, _: &CompiledRateLimit) -> bool {
            self.within_rate
        }
    }

    fn endpoint(
        id: i64,
        owner: i64,
        host: &str,
        claim_id: i64,
        prefix: Option<&str>,
        access: Vec<AccessPolicy>,
    ) -> Endpoint {
        Endpoint {
            id,
            owner: ProductRef {
                kind: ProductKind::Environment,
                id: owner,
            },
            target: EndpointTarget::Workload {
                workload_id: id,
                revision: 1,
                port: 3000,
            },
            hosts: vec![HostClaim {
                host: host.to_string(),
                kind: HostClaimKind::CustomDomain,
                claim_id,
            }],
            path_prefix: prefix.map(str::to_string),
            protocol: EndpointProtocol::Http,
            server_requires_tls: false,
            access,
        }
    }

    fn ip_rules(rules: Vec<(&str, bool)>, unresolved: UnresolvedClient) -> AccessPolicy {
        AccessPolicy::IpRules {
            rules: rules
                .into_iter()
                .map(|(c, allow)| IpRule {
                    cidr: c.to_string(),
                    allow,
                })
                .collect(),
            unresolved,
        }
    }

    fn rate_limit(exempt: Vec<&str>) -> AccessPolicy {
        AccessPolicy::RateLimit {
            limits: vec![RateLimit {
                requests: 10,
                per_secs: 1,
            }],
            exempt: exempt.into_iter().map(str::to_string).collect(),
            scope: RateLimitScope::Endpoint,
        }
    }

    #[test]
    fn evaluation_resumes_after_the_policy_that_decided() {
        let e = CompiledEndpoint::compile(&endpoint(
            1,
            10,
            "shop.example.test",
            1,
            None,
            vec![
                rate_limit(vec![]),
                AccessPolicy::Captcha,
                AccessPolicy::Password { password_ref: 4 },
            ],
        ))
        .unwrap();
        let ctx = Ctx::default();
        assert_eq!(
            e.evaluate_from(0, &ctx),
            (Decision::Challenge(Challenge::Captcha), 1)
        );
        // The captcha turned out to be solved: resume after it.
        assert_eq!(
            e.evaluate_from(2, &ctx),
            (
                Decision::Challenge(Challenge::Password { password_ref: 4 }),
                2
            )
        );
        let ctx = Ctx {
            password: true,
            ..Ctx::default()
        };
        assert_eq!(e.evaluate_from(2, &ctx), (Decision::Allow, 3));
        assert_eq!(e.evaluate_from(9, &ctx), (Decision::Allow, 3));
    }

    #[test]
    fn routes_by_host_case_port_and_longest_prefix() {
        let routes = CompiledRoutes::compile(&[
            endpoint(
                1,
                10,
                "shop.example.test",
                1,
                None,
                vec![AccessPolicy::Public],
            ),
            endpoint(
                2,
                10,
                "shop.example.test",
                2,
                Some("/api"),
                vec![AccessPolicy::Public],
            ),
            endpoint(
                3,
                10,
                "shop.example.test",
                3,
                Some("/api/v2"),
                vec![AccessPolicy::Public],
            ),
        ])
        .unwrap();
        let id = |host, path| routes.route(host, path).map(|e| e.id);
        assert_eq!(id("Shop.Example.TEST:8443", "/"), Some(1));
        assert_eq!(id("shop.example.test.", "/api"), Some(2));
        assert_eq!(id("shop.example.test", "/api/users?x=1"), Some(2));
        assert_eq!(id("shop.example.test", "/api?x=1"), Some(2));
        assert_eq!(id("shop.example.test", "/api/v2/users"), Some(3));
        assert_eq!(
            id("shop.example.test", "/apix"),
            Some(1),
            "prefixes match whole segments"
        );
        assert_eq!(id("other.example.test", "/"), None);
        assert_eq!(id("[::1]:443", "/"), None);
        assert!(routes.conflicts().is_empty());

        let owner = ProductRef {
            kind: ProductKind::Environment,
            id: 10,
        };
        let by_owner = |path| routes.for_owner(owner, path).map(|e| e.id);
        assert_eq!(by_owner("/api/v2/x"), Some(3));
        assert_eq!(by_owner("/api"), Some(2));
        assert_eq!(by_owner("/"), Some(1));
        assert_eq!(
            routes.for_owner(
                ProductRef {
                    kind: ProductKind::Sandbox,
                    id: 10
                },
                "/"
            ),
            None,
            "owners are keyed by kind and id"
        );
    }

    #[test]
    fn exact_hosts_beat_wildcards_which_cover_one_label() {
        let routes = CompiledRoutes::compile(&[
            endpoint(
                1,
                10,
                "*.apps.example.test",
                1,
                None,
                vec![AccessPolicy::Public],
            ),
            endpoint(
                2,
                11,
                "admin.apps.example.test",
                2,
                None,
                vec![AccessPolicy::Public],
            ),
        ])
        .unwrap();
        assert_eq!(
            routes.route("shop.apps.example.test", "/").map(|e| e.id),
            Some(1)
        );
        assert_eq!(
            routes.route("admin.apps.example.test", "/").map(|e| e.id),
            Some(2)
        );
        assert_eq!(
            routes.route("a.b.apps.example.test", "/").map(|e| e.id),
            None
        );
        assert_eq!(routes.route("apps.example.test", "/").map(|e| e.id), None);
    }

    #[test]
    fn a_host_claimed_by_two_products_goes_to_the_first_claim() {
        let routes = CompiledRoutes::compile(&[
            endpoint(
                5,
                11,
                "shop.example.test",
                9,
                None,
                vec![AccessPolicy::Public],
            ),
            endpoint(
                4,
                10,
                "shop.example.test",
                3,
                Some("/api"),
                vec![AccessPolicy::Public],
            ),
        ])
        .unwrap();
        assert_eq!(
            routes.route("shop.example.test", "/api").map(|e| e.id),
            Some(4)
        );
        assert_eq!(
            routes.route("shop.example.test", "/").map(|e| e.id),
            None,
            "the loser is not routed"
        );
        assert_eq!(
            routes.conflicts(),
            [ClaimConflict {
                host: "shop.example.test".into(),
                path_prefix: "/".into(),
                winner_endpoint: 4,
                loser_endpoint: 5
            }]
        );
    }

    #[test]
    fn the_same_host_and_prefix_twice_keeps_the_lower_endpoint() {
        let routes = CompiledRoutes::compile(&[
            endpoint(
                8,
                10,
                "shop.example.test",
                2,
                None,
                vec![AccessPolicy::Public],
            ),
            endpoint(
                6,
                10,
                "shop.example.test",
                1,
                None,
                vec![AccessPolicy::Public],
            ),
        ])
        .unwrap();
        assert_eq!(
            routes.route("shop.example.test", "/").map(|e| e.id),
            Some(6)
        );
        assert_eq!(routes.conflicts().len(), 1);
        assert_eq!(routes.conflicts()[0].loser_endpoint, 8);
    }

    #[test]
    fn blue_green_is_one_write_to_the_target() {
        let mut e = endpoint(
            1,
            10,
            "shop.example.test",
            1,
            None,
            vec![AccessPolicy::Public],
        );
        let blue = CompiledRoutes::compile(std::slice::from_ref(&e)).unwrap();
        e.target = EndpointTarget::Workload {
            workload_id: 1,
            revision: 2,
            port: 3000,
        };
        let green = CompiledRoutes::compile(&[e]).unwrap();
        let revision = |r: &CompiledRoutes| match r.route("shop.example.test", "/").unwrap().target
        {
            EndpointTarget::Workload { revision, .. } => revision,
            _ => 0,
        };
        assert_eq!((revision(&blue), revision(&green)), (1, 2));
    }

    fn decide(access: Vec<AccessPolicy>, ctx: &Ctx) -> Decision<'static> {
        let routes = Box::leak(Box::new(
            CompiledRoutes::compile(&[endpoint(1, 10, "shop.example.test", 1, None, access)])
                .unwrap(),
        ));
        let e = routes.route("shop.example.test", "/").unwrap();
        routes.evaluate(e, ctx)
    }

    #[test]
    fn policies_run_in_order_and_all_must_pass() {
        let access = vec![
            rate_limit(vec![]),
            AccessPolicy::Password { password_ref: 4 },
            AccessPolicy::Captcha,
        ];
        let mut ctx = Ctx {
            within_rate: false,
            ..Ctx::default()
        };
        assert_eq!(
            decide(access.clone(), &ctx),
            Decision::TooManyRequests,
            "rate limited before the password check"
        );
        ctx.within_rate = true;
        assert_eq!(
            decide(access.clone(), &ctx),
            Decision::Challenge(Challenge::Password { password_ref: 4 })
        );
        ctx.password = true;
        assert_eq!(
            decide(access.clone(), &ctx),
            Decision::Challenge(Challenge::Captcha)
        );
        ctx.captcha = true;
        assert_eq!(decide(access, &ctx), Decision::Allow);
    }

    #[test]
    fn exempt_and_unresolved_clients_are_not_counted() {
        let over = Ctx {
            within_rate: false,
            ..Ctx::default()
        };
        assert_eq!(
            decide(vec![rate_limit(vec!["192.0.2.0/24"])], &over),
            Decision::Allow,
            "an exempt client is never limited"
        );
        assert_eq!(
            decide(vec![rate_limit(vec!["198.51.100.0/24"])], &over),
            Decision::TooManyRequests
        );
        let unresolved = Ctx {
            ip: None,
            within_rate: false,
            ..Ctx::default()
        };
        assert_eq!(
            decide(vec![rate_limit(vec![])], &unresolved),
            Decision::Allow,
            "nothing to attribute an unknown client to"
        );
    }

    #[test]
    fn the_endpoint_reports_its_policy_kinds() {
        let routes = CompiledRoutes::compile(&[endpoint(
            1,
            10,
            "shop.example.test",
            1,
            None,
            vec![
                AccessPolicy::Password { password_ref: 4 },
                AccessPolicy::TempsLogin {
                    permission: "projects:read".into(),
                },
            ],
        )])
        .unwrap();
        let e = &routes.endpoints()[0];
        assert!(e.has(PolicyKind::Password));
        assert!(e.has(PolicyKind::TempsLogin));
        assert!(!e.has(PolicyKind::Captcha));
        assert_eq!(e.password_ref(), Some(4));
        assert_eq!(e.login_permission(), Some("projects:read"));
        assert_eq!(e.path_prefix(), "/");
    }

    #[test]
    fn temps_login_and_signed_links_need_their_grant() {
        let login = vec![AccessPolicy::TempsLogin {
            permission: "sandboxes:read".into(),
        }];
        assert_eq!(
            decide(login.clone(), &Ctx::default()),
            Decision::Challenge(Challenge::TempsLogin {
                permission: "sandboxes:read"
            })
        );
        assert_eq!(
            decide(
                login.clone(),
                &Ctx {
                    login: Some("projects:read"),
                    ..Ctx::default()
                }
            ),
            Decision::Challenge(Challenge::TempsLogin {
                permission: "sandboxes:read"
            })
        );
        assert_eq!(
            decide(
                login,
                &Ctx {
                    login: Some("sandboxes:read"),
                    ..Ctx::default()
                }
            ),
            Decision::Allow
        );
        assert_eq!(
            decide(vec![AccessPolicy::SignedLink], &Ctx::default()),
            Decision::Deny(DenyReason::SignedLinkRequired)
        );
    }

    #[test]
    fn ip_rules_match_first_and_an_allow_list_refuses_the_rest() {
        let deny = UnresolvedClient::Deny;
        let ctx = Ctx::default(); // 192.0.2.10
        assert_eq!(
            decide(vec![ip_rules(vec![("192.0.2.0/24", true)], deny)], &ctx),
            Decision::Allow
        );
        assert_eq!(
            decide(vec![ip_rules(vec![("198.51.100.0/24", true)], deny)], &ctx),
            Decision::Deny(DenyReason::IpNotAllowed)
        );
        assert_eq!(
            decide(
                vec![ip_rules(
                    vec![("192.0.2.10/32", false), ("0.0.0.0/0", true)],
                    deny
                )],
                &ctx
            ),
            Decision::Deny(DenyReason::IpNotAllowed)
        );
        assert_eq!(
            decide(vec![ip_rules(vec![("198.51.100.0/24", false)], deny)], &ctx),
            Decision::Allow,
            "a deny list admits the rest"
        );
        let v6 = Ctx {
            ip: Some("2001:db8::1".parse().unwrap()),
            ..Ctx::default()
        };
        assert_eq!(
            decide(vec![ip_rules(vec![("0.0.0.0/0", true)], deny)], &v6),
            Decision::Deny(DenyReason::IpNotAllowed),
            "v4 blocks never match v6"
        );
        assert_eq!(
            decide(vec![ip_rules(vec![("2001:db8::/32", true)], deny)], &v6),
            Decision::Allow
        );
    }

    #[test]
    fn an_unresolved_client_fails_closed_unless_the_policy_admits_it() {
        let unresolved = Ctx {
            ip: None,
            ..Ctx::default()
        };
        assert_eq!(
            decide(
                vec![ip_rules(
                    vec![("198.51.100.0/24", false)],
                    UnresolvedClient::Deny
                )],
                &unresolved
            ),
            Decision::Deny(DenyReason::IpNotAllowed)
        );
        assert_eq!(
            decide(
                vec![ip_rules(
                    vec![("198.51.100.0/24", false)],
                    UnresolvedClient::Admit
                )],
                &unresolved
            ),
            Decision::Allow
        );
    }

    #[test]
    fn geo_allow_and_deny_lists() {
        let geo = |allow: &[&str], deny: &[&str]| {
            vec![AccessPolicy::Geo {
                allow: allow.iter().map(|s| s.to_string()).collect(),
                deny: deny.iter().map(|s| s.to_string()).collect(),
            }]
        };
        let es = Ctx::default();
        assert_eq!(
            decide(geo(&["ES", "pt"], &[]), &es),
            Decision::Allow,
            "codes compare in any case"
        );
        assert_eq!(
            decide(geo(&["FR"], &[]), &es),
            Decision::Deny(DenyReason::CountryNotAllowed)
        );
        assert_eq!(
            decide(geo(&[], &["es"]), &es),
            Decision::Deny(DenyReason::CountryNotAllowed)
        );
        let unknown = Ctx {
            country: None,
            ..Ctx::default()
        };
        assert_eq!(decide(geo(&[], &["ES"]), &unknown), Decision::Allow);
        assert_eq!(
            decide(geo(&["ES"], &[]), &unknown),
            Decision::Deny(DenyReason::CountryNotAllowed),
            "unknown is not in an allow list"
        );
        let bad = endpoint(1, 1, "shop.example.test", 1, None, geo(&["E"], &[]));
        assert!(matches!(
            CompiledEndpoint::compile(&bad),
            Err(AdmissionError::InvalidCountry { .. })
        ));
    }

    #[test]
    fn cidrs_parse_strictly() {
        assert!(parse_cidr("10.0.0.0/8").is_some());
        assert!(parse_cidr("10.0.0.1/8").is_none());
        assert!(parse_cidr("10.0.0.0/33").is_none());
        assert!(parse_cidr("::/0").is_some());
        assert!(parse_cidr("10.0.0.0").is_none());
        let all_v4 = parse_cidr("0.0.0.0/0").unwrap();
        assert!(all_v4.matches("203.0.113.9".parse().unwrap()));
        assert!(!all_v4.matches("::ffff:203.0.113.9".parse().unwrap()));
    }

    #[test]
    fn hand_written_entries_canonicalise_to_strict_blocks() {
        assert_eq!(
            canonical_cidr(" 203.0.113.9 ").as_deref(),
            Some("203.0.113.9/32")
        );
        assert_eq!(
            canonical_cidr("::ffff:203.0.113.9").as_deref(),
            Some("203.0.113.9/32"),
            "a bare mapped address is the IPv4 address"
        );
        assert_eq!(
            canonical_cidr("10.1.2.3/8").as_deref(),
            Some("10.0.0.0/8"),
            "host bits are masked off"
        );
        assert_eq!(
            canonical_cidr("2001:db8::1/32").as_deref(),
            Some("2001:db8::/32")
        );
        for bad in ["", "nope", "10.0.0.0/33", "10.0.0.0/x"] {
            assert_eq!(canonical_cidr(bad), None, "{bad}");
        }
        for entry in ["10.1.2.3/8", "203.0.113.9", "2001:db8::1/32"] {
            let canonical = canonical_cidr(entry).unwrap();
            assert!(parse_cidr(&canonical).is_some(), "{canonical}");
        }
    }
}
