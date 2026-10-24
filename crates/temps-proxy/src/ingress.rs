// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The proxy's access decision for an environment request (ADR-055 §11).
//!
//! Two evaluators produce the same [`AccessOutcome`]:
//!
//! - [`legacy_access`] is the chain the proxy has always run: project IP
//!   gate, rate-limit blacklist and limit, attack-mode captcha, password
//!   wall, in that order;
//! - [`decide`] runs a compiled ingress endpoint from the route snapshot,
//!   whose derived policies restate that chain and whose stored policies
//!   add to it (an IP list, a Temps login).
//!
//! An environment's `ingress_mode` picks which one decides: `legacy` runs
//! only the first, `enforce` only the second, and `shadow` decides with the
//! first while computing the second from the facts the first recorded (it
//! never counts a request against a rate limit twice), counting every
//! disagreement.
//!
//! Everything here is synchronous and allocation-free; the one fact that
//! needs an asynchronous read (whether a captcha was completed) is asked
//! for by returning [`Step::NeedsCaptcha`], so the caller awaits it and
//! resumes after the policy that asked. The proxy writes the response for
//! an outcome; this module only decides it.

use std::cell::Cell;
use std::net::IpAddr;

use pingora_http::RequestHeader;
pub use temps_workload_ingress::grant::LoginScheme;
use temps_workload_ingress::grant::{self, GrantExpectation, GrantKey, GrantUse, Revocations};
use temps_workload_ingress::{
    Challenge, CompiledEndpoint, CompiledRateLimit, Decision, DenyReason, ProductKind, ProductRef,
    RateLimitScope as EndpointScope, RequestContext,
};

use crate::rate_limiter::{
    RateLimitDecision, RateLimitPolicy, RateLimitScope, RateLimitWindow, RateLimiter,
    ResolvedRateLimit,
};

/// Path prefix the proxy answers itself for a `TempsLogin` endpoint: the
/// console's redirect lands here with an exchange grant.
pub const GRANT_EXCHANGE_PATH: &str = grant::EXCHANGE_PATH;

/// Cookie carrying a session grant for a `TempsLogin` endpoint.
pub const SESSION_COOKIE_NAME: &str = "_temps_ingress";

/// What the proxy does with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessOutcome {
    Allow,
    /// The project IP gate (or a stored IP list) refuses the client.
    IpForbidden,
    /// The client is on a rate-limit blacklist.
    Blacklisted,
    TooManyRequests {
        window: RateLimitWindow,
        retry_after_secs: u64,
    },
    /// Attack mode needs a TLS fingerprint and the request has none.
    HttpsRequired,
    CaptchaChallenge,
    PasswordWall,
    /// A `TempsLogin` policy has no valid session grant: redirect to the
    /// console.
    LoginRequired,
    /// Refused for a reason with no legacy equivalent (country, signed link).
    Forbidden,
    /// The environment enforces, but no compiled policy decides this
    /// request (see [`MissingPolicy`]); refused rather than let through.
    Unavailable,
}

impl AccessOutcome {
    /// A stable label for logs and metrics.
    pub fn label(&self) -> &'static str {
        match self {
            AccessOutcome::Allow => "allow",
            AccessOutcome::IpForbidden => "ip_forbidden",
            AccessOutcome::Blacklisted => "rate_limit_blacklisted",
            AccessOutcome::TooManyRequests { .. } => "too_many_requests",
            AccessOutcome::HttpsRequired => "https_required",
            AccessOutcome::CaptchaChallenge => "captcha_challenge",
            AccessOutcome::PasswordWall => "password_wall",
            AccessOutcome::LoginRequired => "login_required",
            AccessOutcome::Forbidden => "forbidden",
            AccessOutcome::Unavailable => "unavailable",
        }
    }

    /// The response a client sees, which is what the two evaluators must
    /// agree on. The IP gate and a blacklist answer the same generic 403,
    /// so they are one class; a 429's `Retry-After` is part of its class.
    pub fn response_class(&self) -> ResponseClass {
        match self {
            AccessOutcome::Allow => ResponseClass::Allow,
            AccessOutcome::IpForbidden | AccessOutcome::Blacklisted | AccessOutcome::Forbidden => {
                ResponseClass::Forbidden
            }
            AccessOutcome::TooManyRequests {
                retry_after_secs, ..
            } => ResponseClass::TooManyRequests {
                retry_after_secs: *retry_after_secs,
            },
            AccessOutcome::HttpsRequired => ResponseClass::HttpsRequired,
            AccessOutcome::CaptchaChallenge => ResponseClass::Captcha,
            AccessOutcome::PasswordWall => ResponseClass::Password,
            AccessOutcome::LoginRequired => ResponseClass::Login,
            AccessOutcome::Unavailable => ResponseClass::Unavailable,
        }
    }
}

/// What a client observes, coarser than [`AccessOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseClass {
    Allow,
    Forbidden,
    TooManyRequests { retry_after_secs: u64 },
    HttpsRequired,
    Captcha,
    Password,
    Login,
    Unavailable,
}

/// The password wall's facts, read from the request before deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordFacts {
    /// The request is the wall's verify `POST`, which the wall always
    /// handles (whatever cookie it carries).
    pub verify_post: bool,
    /// A password cookie for this environment validates.
    pub cookie_valid: bool,
}

impl PasswordFacts {
    fn admits(&self) -> bool {
        self.cookie_valid && !self.verify_post
    }
}

/// The legacy chain's inputs for one request.
#[derive(Debug, Clone, Copy)]
pub struct LegacyRequest<'a> {
    /// Parsed and normalised (IPv4-mapped read as IPv4).
    pub client_ip: Option<IpAddr>,
    /// The project IP gate refuses this client.
    pub ip_restricted: bool,
    pub rate_limit: Option<ResolvedRateLimit<'a>>,
    /// `environment.attack_mode` or else `project.attack_mode`.
    pub attack_mode: bool,
    /// The request is for the captcha's own assets or verification.
    pub captcha_path: bool,
    pub has_tls_fingerprint: bool,
    /// `None` when the environment has no password wall.
    pub password: Option<PasswordFacts>,
}

/// What the legacy chain learned on the way, for a shadow evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LegacyFacts {
    /// The rate limiter's answer, when the chain asked it.
    pub rate: Option<RateLimitDecision>,
    /// The captcha cache's answer, when the chain asked it.
    pub captcha_completed: Option<bool>,
}

/// A step of an evaluation that may need the captcha fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Done(AccessOutcome),
    /// Read whether this client completed the captcha; when it did, resume
    /// with the policies from `resume_at`.
    NeedsCaptcha {
        resume_at: usize,
    },
}

/// The legacy chain up to the captcha check. When the result is
/// [`Step::NeedsCaptcha`], the caller reads the fact and calls
/// [`legacy_after_captcha`].
pub fn legacy_access(
    request: &LegacyRequest<'_>,
    limiter: &RateLimiter,
    now_secs: u64,
    facts: &mut LegacyFacts,
) -> Step {
    if request.ip_restricted {
        return Step::Done(AccessOutcome::IpForbidden);
    }
    if let (Some(ip), Some(resolved)) = (request.client_ip, request.rate_limit.as_ref()) {
        let decision = resolved.check(limiter, ip, now_secs);
        facts.rate = Some(decision);
        match decision {
            RateLimitDecision::Allow => {}
            RateLimitDecision::Blacklisted => return Step::Done(AccessOutcome::Blacklisted),
            RateLimitDecision::Limited {
                window,
                retry_after_secs,
            } => {
                return Step::Done(AccessOutcome::TooManyRequests {
                    window,
                    retry_after_secs,
                })
            }
        }
    }
    if request.attack_mode && !request.captcha_path {
        if !request.has_tls_fingerprint {
            return Step::Done(AccessOutcome::HttpsRequired);
        }
        return Step::NeedsCaptcha { resume_at: 0 };
    }
    Step::Done(legacy_password(request))
}

/// The rest of the legacy chain once the captcha fact is known.
pub fn legacy_after_captcha(
    request: &LegacyRequest<'_>,
    completed: bool,
    facts: &mut LegacyFacts,
) -> AccessOutcome {
    facts.captcha_completed = Some(completed);
    if !completed {
        return AccessOutcome::CaptchaChallenge;
    }
    legacy_password(request)
}

fn legacy_password(request: &LegacyRequest<'_>) -> AccessOutcome {
    match request.password {
        Some(password) if !password.admits() => AccessOutcome::PasswordWall,
        _ => AccessOutcome::Allow,
    }
}

/// Where a compiled rate limit's counts come from.
#[derive(Clone, Copy)]
pub enum RateFacts<'a> {
    /// Count against the proxy's limiter (enforce). The same counters the
    /// legacy chain uses, under the same keys, so a mode switch neither
    /// resets nor doubles a client's budget.
    Live {
        limiter: &'a RateLimiter,
        now_secs: u64,
    },
    /// Replay what the legacy chain's limiter said (shadow).
    Recorded(Option<RateLimitDecision>),
}

/// The request facts a compiled endpoint asks for, all in memory.
pub struct LiveContext<'a> {
    pub client_ip: Option<IpAddr>,
    /// Captcha assets and verification, or a completion already read.
    pub captcha_passed: bool,
    /// Whether the password wall `password_ref` admits the request.
    pub password_grant: &'a dyn Fn(i64) -> bool,
    /// Whether the request carries a session grant for the endpoint with
    /// this permission.
    pub login_grant: &'a dyn Fn(&CompiledEndpoint, &str) -> bool,
    pub rate: RateFacts<'a>,
    /// The limiter's last answer, for `Retry-After`.
    rate_decision: Cell<Option<RateLimitDecision>>,
}

impl<'a> LiveContext<'a> {
    pub fn new(
        client_ip: Option<IpAddr>,
        captcha_passed: bool,
        password_grant: &'a dyn Fn(i64) -> bool,
        login_grant: &'a dyn Fn(&CompiledEndpoint, &str) -> bool,
        rate: RateFacts<'a>,
    ) -> Self {
        Self {
            client_ip,
            captcha_passed,
            password_grant,
            login_grant,
            rate,
            rate_decision: Cell::new(None),
        }
    }
}

/// The limiter key a compiled rate limit counts under: the legacy keys.
fn limiter_scope(endpoint: &CompiledEndpoint, limit: &CompiledRateLimit) -> RateLimitScope {
    match limit.scope() {
        EndpointScope::Instance => RateLimitScope::Instance,
        EndpointScope::Endpoint => match (endpoint.owner.kind, i32::try_from(endpoint.owner.id)) {
            (ProductKind::Environment, Ok(id)) => RateLimitScope::Environment(id),
            // Only environment endpoints are compiled into the proxy's
            // snapshot; anything else counts in the instance budget rather
            // than escaping the limit.
            _ => RateLimitScope::Instance,
        },
    }
}

impl RequestContext for LiveContext<'_> {
    fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    fn country(&self) -> Option<[u8; 2]> {
        None
    }

    fn has_password_grant(&self, _: &CompiledEndpoint, password_ref: i64) -> bool {
        (self.password_grant)(password_ref)
    }

    fn has_login_grant(&self, endpoint: &CompiledEndpoint, permission: &str) -> bool {
        (self.login_grant)(endpoint, permission)
    }

    fn has_signed_link(&self, _: &CompiledEndpoint) -> bool {
        false
    }

    fn captcha_passed(&self, _: &CompiledEndpoint) -> bool {
        self.captcha_passed
    }

    fn within_rate_limit(&self, endpoint: &CompiledEndpoint, limit: &CompiledRateLimit) -> bool {
        let decision = match self.rate {
            RateFacts::Live { limiter, now_secs } => {
                let Some(ip) = self.client_ip else {
                    return true;
                };
                // Exemptions and blacklists are policies of their own; the
                // limiter only counts.
                let policy = RateLimitPolicy {
                    per_minute: limit.window(60).unwrap_or(0),
                    per_hour: limit.window(3600).unwrap_or(0),
                    whitelist: &[],
                    blacklist: &[],
                };
                limiter.check(limiter_scope(endpoint, limit), ip, &policy, now_secs)
            }
            // The legacy chain did not reach its limiter: nothing to replay.
            RateFacts::Recorded(None) => return true,
            RateFacts::Recorded(Some(decision)) => decision,
        };
        self.rate_decision.set(Some(decision));
        decision == RateLimitDecision::Allow
    }
}

/// Runs `endpoint`'s policies from `start`. Allocation-free.
pub fn decide(
    endpoint: &CompiledEndpoint,
    ctx: &LiveContext<'_>,
    start: usize,
    has_tls_fingerprint: bool,
) -> Step {
    let (decision, at) = endpoint.evaluate_from(start, ctx);
    Step::Done(match decision {
        Decision::Allow => AccessOutcome::Allow,
        Decision::Deny(DenyReason::IpNotAllowed) => AccessOutcome::IpForbidden,
        Decision::Deny(DenyReason::CountryNotAllowed | DenyReason::SignedLinkRequired) => {
            AccessOutcome::Forbidden
        }
        Decision::TooManyRequests => match ctx.rate_decision.get() {
            Some(RateLimitDecision::Limited {
                window,
                retry_after_secs,
            }) => AccessOutcome::TooManyRequests {
                window,
                retry_after_secs,
            },
            // A limit decided without a recorded window cannot happen; a
            // minute is the conservative answer.
            _ => AccessOutcome::TooManyRequests {
                window: RateLimitWindow::Minute,
                retry_after_secs: 60,
            },
        },
        Decision::Challenge(Challenge::Captcha) => {
            if !has_tls_fingerprint {
                AccessOutcome::HttpsRequired
            } else {
                return Step::NeedsCaptcha { resume_at: at + 1 };
            }
        }
        Decision::Challenge(Challenge::Password { .. }) => AccessOutcome::PasswordWall,
        Decision::Challenge(Challenge::TempsLogin { .. }) => AccessOutcome::LoginRequired,
    })
}

/// The shadow evaluation: the endpoint decided from what the legacy chain
/// recorded. Counts nothing.
pub fn shadow_outcome(
    endpoint: &CompiledEndpoint,
    client_ip: Option<IpAddr>,
    captcha_path: bool,
    has_tls_fingerprint: bool,
    facts: &LegacyFacts,
    password_grant: &dyn Fn(i64) -> bool,
    login_grant: &dyn Fn(&CompiledEndpoint, &str) -> bool,
) -> AccessOutcome {
    let ctx = LiveContext::new(
        client_ip,
        captcha_path,
        password_grant,
        login_grant,
        RateFacts::Recorded(facts.rate),
    );
    match decide(endpoint, &ctx, 0, has_tls_fingerprint) {
        Step::Done(outcome) => outcome,
        Step::NeedsCaptcha { resume_at } => {
            if facts.captcha_completed != Some(true) {
                return AccessOutcome::CaptchaChallenge;
            }
            let ctx = LiveContext::new(
                client_ip,
                true,
                password_grant,
                login_grant,
                RateFacts::Recorded(facts.rate),
            );
            match decide(endpoint, &ctx, resume_at, has_tls_fingerprint) {
                Step::Done(outcome) => outcome,
                Step::NeedsCaptcha { .. } => AccessOutcome::CaptchaChallenge,
            }
        }
    }
}

/// What a session-grant check needs besides the request.
#[derive(Clone, Copy)]
pub struct GrantFacts<'a> {
    /// `CookieCrypto::ingress_grant_master_key`.
    pub master: &'a [u8; 32],
    pub revocations: &'a Revocations,
    /// The request host as [`grant_host`] reads it; `None` refuses every
    /// grant.
    pub host: Option<&'a str>,
    pub now_secs: u64,
}

/// The values of every `name` cookie on the request, without allocating.
pub fn cookie_values<'r>(
    request: &'r RequestHeader,
    name: &'r str,
) -> impl Iterator<Item = &'r str> + 'r {
    request
        .headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|field| field.to_str().ok())
        .flat_map(|field| field.split(';'))
        .filter_map(move |pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value.trim_matches('"'))
        })
}

/// Whether the request carries a session grant for `endpoint` with
/// `permission`: signed by the endpoint owner's key, for this host,
/// unexpired and not revoked.
pub fn session_grant_admits(
    request: &RequestHeader,
    endpoint: &CompiledEndpoint,
    permission: &str,
    grants: &GrantFacts<'_>,
) -> bool {
    let Some(host) = grants.host else {
        return false;
    };
    let key = GrantKey::derive(grants.master, endpoint.owner);
    let expected = GrantExpectation {
        owner: endpoint.owner,
        host,
        permission,
        usage: GrantUse::Session,
        now: grants.now_secs,
    };
    cookie_values(request, SESSION_COOKIE_NAME)
        .any(|token| grant::admits(&key, token, &expected, grants.revocations))
}

/// One environment request, as the compiled endpoint reads it.
#[derive(Clone, Copy)]
pub struct EndpointRequest<'a> {
    pub request: &'a RequestHeader,
    pub environment_id: i32,
    pub client_ip: Option<IpAddr>,
    pub captcha_path: bool,
    pub has_tls_fingerprint: bool,
    /// The environment's own password wall, when it has one.
    pub password: Option<PasswordFacts>,
    pub grants: GrantFacts<'a>,
}

impl EndpointRequest<'_> {
    fn password_grant(&self, password_ref: i64) -> bool {
        password_ref == i64::from(self.environment_id)
            && self.password.is_some_and(|password| password.admits())
    }
}

/// Why an enforcing environment's request has no compiled endpoint to
/// decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingPolicy {
    /// No ingress load has completed yet (the snapshot is the empty
    /// default), so no environment's policy is known.
    NotLoaded,
    /// One of the environment's endpoints did not compile, or stores a
    /// policy kind this phase does not enforce.
    Broken,
    /// The snapshot has no endpoint of this environment covering the path:
    /// a failed reload kept an older snapshot from before it enforced, or
    /// no endpoint row covers the path.
    NoEndpoint,
}

impl MissingPolicy {
    /// A stable label for logs.
    pub fn label(&self) -> &'static str {
        match self {
            MissingPolicy::NotLoaded => "ingress_snapshot_not_loaded",
            MissingPolicy::Broken => "endpoint_broken",
            MissingPolicy::NoEndpoint => "no_endpoint",
        }
    }
}

/// The endpoint that decides an enforcing environment's request at `path`.
///
/// Enforce never falls back to the legacy chain: a stored policy such as a
/// Temps login exists only in the compiled endpoint, so any state that
/// cannot produce the environment's endpoint refuses the request. A broken
/// environment refuses even where another of its endpoints compiled: the
/// broken one may be the more specific path prefix this request belongs to.
pub fn enforcing_endpoint<'s>(
    snapshot: &'s temps_routes::ingress::IngressSnapshot,
    environment_id: i32,
    path: &str,
) -> Result<&'s CompiledEndpoint, MissingPolicy> {
    if !snapshot.is_loaded() {
        return Err(MissingPolicy::NotLoaded);
    }
    if snapshot.is_broken(environment_id) {
        return Err(MissingPolicy::Broken);
    }
    snapshot
        .environment_endpoint(environment_id, path)
        .ok_or(MissingPolicy::NoEndpoint)
}

/// The enforcing evaluation, counting against `limiter`, from policy
/// `start`. `captcha_passed` is the completion the caller read after a
/// [`Step::NeedsCaptcha`].
pub fn enforce_step(
    endpoint: &CompiledEndpoint,
    request: &EndpointRequest<'_>,
    limiter: &RateLimiter,
    captcha_passed: bool,
    start: usize,
) -> Step {
    let password_grant = |password_ref: i64| request.password_grant(password_ref);
    let login_grant = |endpoint: &CompiledEndpoint, permission: &str| {
        session_grant_admits(request.request, endpoint, permission, &request.grants)
    };
    let ctx = LiveContext::new(
        request.client_ip,
        request.captcha_path || captcha_passed,
        &password_grant,
        &login_grant,
        RateFacts::Live {
            limiter,
            now_secs: request.grants.now_secs,
        },
    );
    decide(endpoint, &ctx, start, request.has_tls_fingerprint)
}

/// The shadow evaluation of `request`, from what the legacy chain recorded.
pub fn shadow_step(
    endpoint: &CompiledEndpoint,
    request: &EndpointRequest<'_>,
    facts: &LegacyFacts,
) -> AccessOutcome {
    let password_grant = |password_ref: i64| request.password_grant(password_ref);
    let login_grant = |endpoint: &CompiledEndpoint, permission: &str| {
        session_grant_admits(request.request, endpoint, permission, &request.grants)
    };
    shadow_outcome(
        endpoint,
        request.client_ip,
        request.captcha_path,
        request.has_tls_fingerprint,
        facts,
        &password_grant,
        &login_grant,
    )
}

/// The console page a `TempsLogin` challenge sends the visitor to: it signs
/// them in if needed, checks the permission, and sends them back to
/// [`GRANT_EXCHANGE_PATH`] on `host` with an exchange grant. `None` when the
/// instance's external URL is not an absolute `http(s)` URL.
///
/// The request is signed with the owner's grant key
/// ([`grant::sign_login_request`]), so the console only ever sends a grant
/// back to a host this ingress asked about.
///
/// Runs only on the challenge path, never for an admitted request.
pub fn login_redirect_url(
    external_url: &str,
    master: &[u8; 32],
    owner: ProductRef,
    scheme: LoginScheme,
    host: &str,
    next: &str,
    now_secs: u64,
) -> Option<String> {
    let base = url::Url::parse(external_url.trim()).ok()?;
    if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
        return None;
    }
    let mut buffer = [0u8; 256];
    let host = grant_host(host, &mut buffer)?;
    let signature =
        grant::sign_login_request(&GrantKey::derive(master, owner), scheme, host, now_secs);
    let mut url = base;
    let path = format!("{}/ingress/login", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("owner_kind", owner.kind.as_str())
        .append_pair("owner_id", &owner.id.to_string())
        .append_pair("scheme", scheme.as_str())
        .append_pair("host", host)
        .append_pair("next", sanitize_next(next))
        .append_pair("ts", &now_secs.to_string())
        .append_pair("sig", &signature);
    Some(url.into())
}

/// `next` when it is a path on this host, `/` otherwise: never an absolute
/// or scheme-relative URL, a backslash path or a control character.
pub fn sanitize_next(next: &str) -> &str {
    let safe = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && next.len() <= 2048
        && !next.bytes().any(|b| b.is_ascii_control() || b == b'\\');
    if safe {
        next
    } else {
        "/"
    }
}

/// What the ingress answers a valid exchange with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionExchange {
    /// The `Set-Cookie` value carrying the session grant.
    pub set_cookie: String,
    /// Where to send the visitor: the sanitised `next`.
    pub location: String,
    pub grant_id: i64,
    pub user_id: i64,
}

/// Why an exchange at [`GRANT_EXCHANGE_PATH`] was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExchangeError {
    #[error("exchange request for {owner} carries no grant")]
    MissingGrant { owner: ProductRef },
    #[error("exchange request for {owner} has a host the grant cannot name")]
    UnusableHost { owner: ProductRef },
    #[error("endpoint for {owner} does not require a Temps login")]
    NotLoginProtected { owner: ProductRef },
    #[error("exchange grant for {owner} refused: {source}")]
    Refused {
        owner: ProductRef,
        #[source]
        source: grant::GrantError,
    },
    #[error("exchange grant {grant_id} for {owner} was already traded for a session")]
    AlreadyUsed { owner: ProductRef, grant_id: i64 },
    #[error(
        "exchange for {owner} refused: {capacity} unexpired exchange grants are already \
         recorded on this proxy"
    )]
    ReplayGuardFull { owner: ProductRef, capacity: usize },
}

/// Exchange grants this proxy has already traded for a session, so each
/// is used once.
///
/// An exchange grant rides in a URL, which browsers keep in history and
/// logs can capture. Without this record, anyone who saw the URL within
/// the grant's lifetime could trade it for a session of their own. Each
/// sign-in mints a grant with its own id, so a second exchange of an id is
/// always a replay.
///
/// The record is bounded: an entry leaves once its grant has expired, and
/// a full record refuses new exchanges rather than forgetting live ones.
/// It is taken only on the exchange path, once per sign-in, never per
/// request. It is per process: in a split topology with several proxies a
/// grant could still be traded once on each of them within its lifetime.
#[derive(Debug)]
pub struct ConsumedExchanges {
    used: std::sync::Mutex<std::collections::HashMap<i64, u64>>,
    capacity: usize,
}

impl Default for ConsumedExchanges {
    fn default() -> Self {
        Self::with_capacity(10_000)
    }
}

impl ConsumedExchanges {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            used: std::sync::Mutex::new(std::collections::HashMap::new()),
            capacity,
        }
    }

    /// Records `grant_id` as traded. Refuses an id already recorded, and a
    /// new id when the record is full of unexpired grants.
    fn consume(
        &self,
        owner: ProductRef,
        grant_id: i64,
        expires_at: u64,
        now: u64,
    ) -> Result<(), ExchangeError> {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if used.contains_key(&grant_id) {
            return Err(ExchangeError::AlreadyUsed { owner, grant_id });
        }
        if used.len() >= self.capacity {
            used.retain(|_, expires| *expires > now);
            if used.len() >= self.capacity {
                return Err(ExchangeError::ReplayGuardFull {
                    owner,
                    capacity: self.capacity,
                });
            }
        }
        used.insert(grant_id, expires_at);
        Ok(())
    }
}

/// The `Path` attribute of a session cookie: `prefix` (a path route's, so
/// the target's cookie sits beside the host's own instead of replacing it)
/// when it is a plain absolute path, `/` otherwise. Never lets a prefix
/// inject another cookie attribute.
pub fn session_cookie_path(prefix: Option<&str>) -> &str {
    match prefix {
        Some(prefix)
            if prefix.starts_with('/')
                && prefix.len() <= 512
                && prefix.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b'%')
                }) =>
        {
            prefix
        }
        _ => "/",
    }
}

/// Trades the exchange grant in the query of a request to
/// [`GRANT_EXCHANGE_PATH`] for a session grant cookie (ADR-055 §11).
///
/// `cookie_path` scopes the cookie: the path route's prefix when the
/// exchange is for a path-routed target, so signing in to it does not
/// overwrite the session of the host's own environment.
///
/// The exchange grant must be signed by the endpoint owner's key, for this
/// host and the endpoint's `TempsLogin` permission, unexpired and not
/// revoked. The session grant keeps its id, so revoking the grant row
/// revokes both. Each exchange grant is traded once (`consumed`). Runs once
/// per sign-in, never per request.
pub fn exchange_grant(
    endpoint: &CompiledEndpoint,
    grants: &GrantFacts<'_>,
    consumed: &ConsumedExchanges,
    query: Option<&str>,
    secure: bool,
    cookie_path: Option<&str>,
) -> Result<SessionExchange, ExchangeError> {
    let owner = endpoint.owner;
    let permission = endpoint
        .login_permission()
        .ok_or(ExchangeError::NotLoginProtected { owner })?;
    let host = grants.host.ok_or(ExchangeError::UnusableHost { owner })?;
    let mut token = None;
    let mut next = None;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        match key.as_ref() {
            "grant" if token.is_none() => token = Some(value.into_owned()),
            "next" if next.is_none() => next = Some(value.into_owned()),
            _ => {}
        }
    }
    let token = token
        .filter(|token| !token.is_empty())
        .ok_or(ExchangeError::MissingGrant { owner })?;
    let key = GrantKey::derive(grants.master, owner);
    let mut buffer = [0u8; grant::MAX_GRANT_PAYLOAD];
    let claims = grant::verify(&key, &token, &mut buffer)
        .map_err(|source| ExchangeError::Refused { owner, source })?;
    grant::check(
        &claims,
        &GrantExpectation {
            owner,
            host,
            permission,
            usage: GrantUse::Exchange,
            now: grants.now_secs,
        },
        grants.revocations,
    )
    .map_err(|source| ExchangeError::Refused { owner, source })?;
    consumed.consume(owner, claims.grant_id, claims.expires_at, grants.now_secs)?;
    let session = grant::mint(
        &key,
        &grant::GrantClaims {
            usage: GrantUse::Session,
            expires_at: grants.now_secs + grant::SESSION_GRANT_TTL_SECS,
            ..claims
        },
    )
    .map_err(|source| ExchangeError::Refused { owner, source })?;
    let set_cookie = format!(
        "{SESSION_COOKIE_NAME}={session}; Path={}; Max-Age={}; HttpOnly; SameSite=Lax{}",
        session_cookie_path(cookie_path),
        grant::SESSION_GRANT_TTL_SECS,
        if secure { "; Secure" } else { "" }
    );
    Ok(SessionExchange {
        set_cookie,
        location: sanitize_next(next.as_deref().unwrap_or("/")).to_string(),
        grant_id: claims.grant_id,
        user_id: claims.user_id,
    })
}

/// The host as a grant names it: lowercase ASCII, no port, no trailing dot.
/// Writes into `buffer` and returns `None` for a host longer than it.
pub fn grant_host<'b>(host: &str, buffer: &'b mut [u8; 256]) -> Option<&'b str> {
    let host = match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            name
        }
        _ => host,
    };
    let host = host.trim_end_matches('.');
    let bytes = host.as_bytes();
    if bytes.len() > buffer.len() {
        return None;
    }
    for (slot, byte) in buffer.iter_mut().zip(bytes) {
        *slot = byte.to_ascii_lowercase();
    }
    std::str::from_utf8(&buffer[..bytes.len()]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload_ingress::{
        AccessPolicy, Endpoint, EndpointProtocol, EndpointTarget, ProductRef, RateLimit,
    };

    const T0: u64 = 1_800_000_000;

    fn endpoint(access: Vec<AccessPolicy>) -> CompiledEndpoint {
        CompiledEndpoint::compile(&Endpoint {
            id: 1,
            owner: ProductRef {
                kind: ProductKind::Environment,
                id: 7,
            },
            target: EndpointTarget::Product,
            hosts: Vec::new(),
            path_prefix: None,
            protocol: EndpointProtocol::Http,
            server_requires_tls: false,
            access,
        })
        .expect("test endpoint compiles")
    }

    fn raw_endpoint(
        id: i64,
        environment_id: i64,
        path_prefix: Option<&str>,
        access: Vec<AccessPolicy>,
    ) -> Endpoint {
        Endpoint {
            id,
            owner: ProductRef {
                kind: ProductKind::Environment,
                id: environment_id,
            },
            target: EndpointTarget::Product,
            hosts: Vec::new(),
            path_prefix: path_prefix.map(str::to_string),
            protocol: EndpointProtocol::Http,
            server_requires_tls: false,
            access,
        }
    }

    fn login() -> AccessPolicy {
        AccessPolicy::TempsLogin {
            permission: temps_routes::ingress::ENVIRONMENT_LOGIN_PERMISSION.into(),
        }
    }

    fn built(endpoints: Vec<Endpoint>) -> temps_routes::ingress::IngressSnapshot {
        temps_routes::ingress::IngressSnapshot::build(
            endpoints,
            std::collections::HashMap::new(),
            Revocations::default(),
        )
    }

    #[test]
    fn enforcing_refuses_before_any_ingress_load_succeeded() {
        // What the route table publishes when the first ingress load fails:
        // the default snapshot, which must not read as "no restrictions".
        let snapshot = temps_routes::ingress::IngressSnapshot::default();
        assert_eq!(
            enforcing_endpoint(&snapshot, 7, "/").err(),
            Some(MissingPolicy::NotLoaded)
        );
    }

    #[test]
    fn enforcing_refuses_an_environment_the_snapshot_has_no_endpoint_for() {
        // A kept snapshot from before environment 9 enforced: 7 is covered,
        // 9 is not, and must be refused rather than run the legacy chain.
        let snapshot = built(vec![raw_endpoint(1, 7, None, vec![login()])]);
        assert!(enforcing_endpoint(&snapshot, 7, "/").is_ok());
        assert_eq!(
            enforcing_endpoint(&snapshot, 9, "/").err(),
            Some(MissingPolicy::NoEndpoint)
        );
    }

    #[test]
    fn enforcing_refuses_a_path_no_endpoint_covers() {
        let snapshot = built(vec![raw_endpoint(1, 7, Some("/api"), vec![login()])]);
        assert!(enforcing_endpoint(&snapshot, 7, "/api/users").is_ok());
        assert_eq!(
            enforcing_endpoint(&snapshot, 7, "/").err(),
            Some(MissingPolicy::NoEndpoint)
        );
    }

    #[test]
    fn a_broken_endpoint_refuses_its_environment_even_where_another_compiled() {
        // The `/api` endpoint does not compile; the whole-host one does.
        // `/api` must not fall through to the whole-host endpoint.
        let snapshot = built(vec![
            raw_endpoint(1, 7, None, vec![AccessPolicy::Public]),
            raw_endpoint(
                2,
                7,
                Some("/api"),
                vec![AccessPolicy::Geo {
                    allow: vec!["not-a-country".into()],
                    deny: vec![],
                }],
            ),
        ]);
        assert_eq!(
            enforcing_endpoint(&snapshot, 7, "/api/users").err(),
            Some(MissingPolicy::Broken)
        );
        assert_eq!(
            enforcing_endpoint(&snapshot, 7, "/").err(),
            Some(MissingPolicy::Broken)
        );
    }

    fn no_password(_: i64) -> bool {
        false
    }

    fn no_login(_: &CompiledEndpoint, _: &str) -> bool {
        false
    }

    fn ip(s: &str) -> Option<IpAddr> {
        s.parse().ok()
    }

    #[test]
    fn a_rate_limit_answers_with_the_limiters_retry_after() {
        let ep = endpoint(vec![AccessPolicy::RateLimit {
            limits: vec![RateLimit {
                requests: 1,
                per_secs: 60,
            }],
            exempt: vec![],
            scope: EndpointScope::Endpoint,
        }]);
        let limiter = RateLimiter::new();
        let ctx = LiveContext::new(
            ip("203.0.113.5"),
            false,
            &no_password,
            &no_login,
            RateFacts::Live {
                limiter: &limiter,
                now_secs: T0 + 10,
            },
        );
        assert_eq!(decide(&ep, &ctx, 0, true), Step::Done(AccessOutcome::Allow));
        assert_eq!(
            decide(&ep, &ctx, 0, true),
            Step::Done(AccessOutcome::TooManyRequests {
                window: RateLimitWindow::Minute,
                retry_after_secs: 50,
            })
        );
    }

    #[test]
    fn a_captcha_asks_for_its_fact_and_resumes_after_it() {
        let ep = endpoint(vec![
            AccessPolicy::Captcha,
            AccessPolicy::Password { password_ref: 7 },
        ]);
        let limiter = RateLimiter::new();
        let live = RateFacts::Live {
            limiter: &limiter,
            now_secs: T0,
        };
        let ctx = LiveContext::new(None, false, &no_password, &no_login, live);
        assert_eq!(
            decide(&ep, &ctx, 0, false),
            Step::Done(AccessOutcome::HttpsRequired)
        );
        assert_eq!(
            decide(&ep, &ctx, 0, true),
            Step::NeedsCaptcha { resume_at: 1 }
        );
        assert_eq!(
            decide(&ep, &ctx, 1, true),
            Step::Done(AccessOutcome::PasswordWall)
        );
        let unlocked = |r: i64| r == 7;
        let ctx = LiveContext::new(None, false, &unlocked, &no_login, live);
        assert_eq!(decide(&ep, &ctx, 1, true), Step::Done(AccessOutcome::Allow));
    }

    #[test]
    fn a_temps_login_needs_its_grant() {
        let ep = endpoint(vec![AccessPolicy::TempsLogin {
            permission: "projects:read".into(),
        }]);
        let limiter = RateLimiter::new();
        let live = RateFacts::Live {
            limiter: &limiter,
            now_secs: T0,
        };
        let ctx = LiveContext::new(None, false, &no_password, &no_login, live);
        assert_eq!(
            decide(&ep, &ctx, 0, true),
            Step::Done(AccessOutcome::LoginRequired)
        );
        let granted = |_: &CompiledEndpoint, p: &str| p == "projects:read";
        let ctx = LiveContext::new(None, false, &no_password, &granted, live);
        assert_eq!(decide(&ep, &ctx, 0, true), Step::Done(AccessOutcome::Allow));
    }

    #[test]
    fn shadow_replays_recorded_facts_and_counts_nothing() {
        let ep = endpoint(vec![
            AccessPolicy::RateLimit {
                limits: vec![RateLimit {
                    requests: 1,
                    per_secs: 60,
                }],
                exempt: vec![],
                scope: EndpointScope::Endpoint,
            },
            AccessPolicy::Captcha,
        ]);
        let limited = LegacyFacts {
            rate: Some(RateLimitDecision::Limited {
                window: RateLimitWindow::Hour,
                retry_after_secs: 99,
            }),
            captcha_completed: None,
        };
        assert_eq!(
            shadow_outcome(
                &ep,
                ip("198.51.100.1"),
                false,
                true,
                &limited,
                &no_password,
                &no_login
            ),
            AccessOutcome::TooManyRequests {
                window: RateLimitWindow::Hour,
                retry_after_secs: 99,
            }
        );
        let solved = LegacyFacts {
            rate: Some(RateLimitDecision::Allow),
            captcha_completed: Some(true),
        };
        assert_eq!(
            shadow_outcome(
                &ep,
                ip("198.51.100.1"),
                false,
                true,
                &solved,
                &no_password,
                &no_login
            ),
            AccessOutcome::Allow
        );
        let unsolved = LegacyFacts {
            captcha_completed: Some(false),
            ..solved
        };
        assert_eq!(
            shadow_outcome(
                &ep,
                ip("198.51.100.1"),
                false,
                true,
                &unsolved,
                &no_password,
                &no_login
            ),
            AccessOutcome::CaptchaChallenge
        );
    }

    #[test]
    fn legacy_chain_order_and_password_verify_post() {
        let limiter = RateLimiter::new();
        let mut facts = LegacyFacts::default();
        let base = LegacyRequest {
            client_ip: ip("192.0.2.9"),
            ip_restricted: false,
            rate_limit: None,
            attack_mode: false,
            captcha_path: false,
            has_tls_fingerprint: true,
            password: Some(PasswordFacts {
                verify_post: true,
                cookie_valid: true,
            }),
        };
        assert_eq!(
            legacy_access(&base, &limiter, T0, &mut facts),
            Step::Done(AccessOutcome::PasswordWall),
            "the verify POST is always the wall's"
        );
        let restricted = LegacyRequest {
            ip_restricted: true,
            ..base
        };
        assert_eq!(
            legacy_access(&restricted, &limiter, T0, &mut facts),
            Step::Done(AccessOutcome::IpForbidden)
        );
        let attack = LegacyRequest {
            attack_mode: true,
            ..base
        };
        assert_eq!(
            legacy_access(&attack, &limiter, T0, &mut facts),
            Step::NeedsCaptcha { resume_at: 0 }
        );
        assert_eq!(
            legacy_after_captcha(&attack, false, &mut facts),
            AccessOutcome::CaptchaChallenge
        );
        assert_eq!(facts.captcha_completed, Some(false));
        let captcha_asset = LegacyRequest {
            captcha_path: true,
            password: None,
            ..attack
        };
        assert_eq!(
            legacy_access(&captcha_asset, &limiter, T0, &mut facts),
            Step::Done(AccessOutcome::Allow)
        );
    }

    #[test]
    fn login_redirects_go_to_the_console_page_with_a_signed_request() {
        let master = [9u8; 32];
        let owner = ProductRef {
            kind: ProductKind::Environment,
            id: 7,
        };
        let url = login_redirect_url(
            "https://console.example.test/",
            &master,
            owner,
            LoginScheme::Https,
            "App.Example.Test:443",
            "/a?b=c",
            T0,
        )
        .expect("valid external url");
        let signature = grant::sign_login_request(
            &GrantKey::derive(&master, owner),
            LoginScheme::Https,
            "app.example.test",
            T0,
        );
        assert_eq!(
            url,
            format!(
                "https://console.example.test/ingress/login?owner_kind=environment&owner_id=7&scheme=https&host=app.example.test&next=%2Fa%3Fb%3Dc&ts={T0}&sig={signature}"
            )
        );
        // An unsafe `next` is replaced, never forwarded.
        let url = login_redirect_url(
            "https://console.example.test/base",
            &master,
            owner,
            LoginScheme::Http,
            "app.example.test",
            "//elsewhere.example.test/",
            T0,
        )
        .expect("valid external url");
        assert!(url.starts_with("https://console.example.test/base/ingress/login?"));
        assert!(url.contains("&next=%2F&"), "{url}");
        for external in ["not a url", "ftp://console.example.test"] {
            assert_eq!(
                login_redirect_url(
                    external,
                    &master,
                    owner,
                    LoginScheme::Https,
                    "app.example.test",
                    "/",
                    T0
                ),
                None
            );
        }
    }

    #[test]
    fn next_must_be_a_path_on_the_same_host() {
        assert_eq!(sanitize_next("/a/b?c=d#e"), "/a/b?c=d#e");
        for unsafe_next in [
            "",
            "https://elsewhere.example.test/",
            "//elsewhere.example.test",
            "/\\elsewhere",
            "/a\\b",
            "/a\nb",
            "relative",
        ] {
            assert_eq!(sanitize_next(unsafe_next), "/", "{unsafe_next:?}");
        }
    }

    fn login_endpoint() -> CompiledEndpoint {
        endpoint(vec![AccessPolicy::TempsLogin {
            permission: "projects:read".into(),
        }])
    }

    fn exchange_token(
        master: &[u8; 32],
        usage: GrantUse,
        host: &str,
        permission: &str,
        expires_at: u64,
    ) -> String {
        let owner = ProductRef {
            kind: ProductKind::Environment,
            id: 7,
        };
        grant::mint(
            &GrantKey::derive(master, owner),
            &grant::GrantClaims {
                grant_id: 12,
                usage,
                owner,
                host,
                permission,
                user_id: 4,
                expires_at,
            },
        )
        .expect("mint")
    }

    #[test]
    fn an_exchange_grant_buys_a_session_cookie_for_its_host() {
        let master = [9u8; 32];
        let none = Revocations::default();
        let facts = GrantFacts {
            master: &master,
            revocations: &none,
            host: Some("app.example.test"),
            now_secs: T0,
        };
        let ep = login_endpoint();
        let token = exchange_token(
            &master,
            GrantUse::Exchange,
            "app.example.test",
            "projects:read",
            T0 + 60,
        );
        let query = format!("grant={token}&next=%2Fdashboard%3Ftab%3D1");
        let consumed = ConsumedExchanges::default();
        let exchange =
            exchange_grant(&ep, &facts, &consumed, Some(&query), true, None).expect("exchange");
        assert_eq!(exchange.location, "/dashboard?tab=1");
        assert_eq!((exchange.grant_id, exchange.user_id), (12, 4));
        assert!(exchange.set_cookie.starts_with("_temps_ingress=tg1."));
        assert!(exchange
            .set_cookie
            .ends_with("; Path=/; Max-Age=43200; HttpOnly; SameSite=Lax; Secure"));

        // The cookie is a session grant the endpoint now admits.
        let cookie = exchange
            .set_cookie
            .split(';')
            .next()
            .expect("cookie pair")
            .to_string();
        let mut request = RequestHeader::build("GET", b"/dashboard", None).expect("request");
        request.append_header("cookie", cookie).expect("cookie");
        assert!(session_grant_admits(&request, &ep, "projects:read", &facts));

        // The same grant cannot be traded again: its URL may have leaked.
        assert!(matches!(
            exchange_grant(&ep, &facts, &consumed, Some(&query), true, None),
            Err(ExchangeError::AlreadyUsed { grant_id: 12, .. })
        ));

        // Over plain HTTP the cookie is not `Secure`; an unsafe next goes home.
        let query = format!("grant={token}&next=https%3A%2F%2Felsewhere.example.test%2F");
        let exchange = exchange_grant(
            &ep,
            &facts,
            &ConsumedExchanges::default(),
            Some(&query),
            false,
            None,
        )
        .expect("exchange");
        assert_eq!(exchange.location, "/");
        assert!(!exchange.set_cookie.contains("Secure"));
    }

    #[test]
    fn a_path_routed_exchange_scopes_its_cookie_to_the_route_prefix() {
        let master = [9u8; 32];
        let none = Revocations::default();
        let facts = GrantFacts {
            master: &master,
            revocations: &none,
            host: Some("app.example.test"),
            now_secs: T0,
        };
        let ep = login_endpoint();
        let token = exchange_token(
            &master,
            GrantUse::Exchange,
            "app.example.test",
            "projects:read",
            T0 + 60,
        );
        let query = format!("grant={token}&next=%2Fapi%2Fusers");
        let exchange = exchange_grant(
            &ep,
            &facts,
            &ConsumedExchanges::default(),
            Some(&query),
            true,
            Some("/api"),
        )
        .expect("exchange");
        assert_eq!(exchange.location, "/api/users");
        assert!(exchange.set_cookie.contains("; Path=/api; "));
    }

    #[test]
    fn a_cookie_path_never_carries_another_attribute() {
        assert_eq!(session_cookie_path(None), "/");
        assert_eq!(session_cookie_path(Some("/api")), "/api");
        assert_eq!(
            session_cookie_path(Some("/api/v1-beta_2")),
            "/api/v1-beta_2"
        );
        for hostile in [
            "api",
            "/api; Domain=evil.example.test",
            "/api\r\nSet-Cookie: x=1",
            "/a b",
            "",
        ] {
            assert_eq!(session_cookie_path(Some(hostile)), "/", "{hostile:?}");
        }
    }

    #[test]
    fn an_exchange_refuses_sessions_other_hosts_expired_revoked_and_unprotected_endpoints() {
        static MASTER: [u8; 32] = [9u8; 32];
        let master = MASTER;
        let none = Revocations::default();
        let revoked = Revocations::new([12]);
        fn facts<'a>(host: Option<&'a str>, revocations: &'a Revocations) -> GrantFacts<'a> {
            GrantFacts {
                master: &MASTER,
                revocations,
                host,
                now_secs: T0,
            }
        }
        let (none_ref, revoked_ref) = (&none, &revoked);
        let ep = login_endpoint();
        let consumed = ConsumedExchanges::default();
        let exchange = |token: &str, facts: &GrantFacts<'_>| {
            exchange_grant(
                &ep,
                facts,
                &consumed,
                Some(&format!("grant={token}")),
                true,
                None,
            )
        };
        let host = Some("app.example.test");
        let session = exchange_token(
            &master,
            GrantUse::Session,
            "app.example.test",
            "projects:read",
            T0 + 60,
        );
        assert!(matches!(
            exchange(&session, &facts(host, none_ref)),
            Err(ExchangeError::Refused {
                source: grant::GrantError::WrongUse { .. },
                ..
            })
        ));
        let valid = exchange_token(
            &master,
            GrantUse::Exchange,
            "app.example.test",
            "projects:read",
            T0 + 60,
        );
        assert!(matches!(
            exchange(&valid, &facts(Some("shop.example.test"), none_ref)),
            Err(ExchangeError::Refused {
                source: grant::GrantError::WrongHost { .. },
                ..
            })
        ));
        assert!(matches!(
            exchange(&valid, &facts(None, none_ref)),
            Err(ExchangeError::UnusableHost { .. })
        ));
        assert!(matches!(
            exchange(&valid, &facts(host, revoked_ref)),
            Err(ExchangeError::Refused {
                source: grant::GrantError::Revoked { grant_id: 12 },
                ..
            })
        ));
        let expired = exchange_token(
            &master,
            GrantUse::Exchange,
            "app.example.test",
            "projects:read",
            T0,
        );
        assert!(matches!(
            exchange(&expired, &facts(host, none_ref)),
            Err(ExchangeError::Refused {
                source: grant::GrantError::Expired { .. },
                ..
            })
        ));
        let other_permission = exchange_token(
            &master,
            GrantUse::Exchange,
            "app.example.test",
            "projects:write",
            T0 + 60,
        );
        assert!(matches!(
            exchange(&other_permission, &facts(host, none_ref)),
            Err(ExchangeError::Refused {
                source: grant::GrantError::WrongPermission { .. },
                ..
            })
        ));
        let forged = exchange_token(
            &[1u8; 32],
            GrantUse::Exchange,
            "app.example.test",
            "projects:read",
            T0 + 60,
        );
        assert!(matches!(
            exchange(&forged, &facts(host, none_ref)),
            Err(ExchangeError::Refused {
                source: grant::GrantError::BadSignature,
                ..
            })
        ));
        assert!(matches!(
            exchange_grant(
                &ep,
                &facts(host, none_ref),
                &consumed,
                Some("next=%2F"),
                true,
                None
            ),
            Err(ExchangeError::MissingGrant { .. })
        ));
        assert!(matches!(
            exchange_grant(
                &endpoint(vec![]),
                &facts(host, none_ref),
                &consumed,
                Some(&format!("grant={valid}")),
                true,
                None
            ),
            Err(ExchangeError::NotLoginProtected { .. })
        ));
    }

    #[test]
    fn the_replay_guard_forgets_expired_grants_and_refuses_when_full_of_live_ones() {
        let owner = ProductRef {
            kind: ProductKind::Environment,
            id: 7,
        };
        let guard = ConsumedExchanges::with_capacity(2);
        guard.consume(owner, 1, T0 + 10, T0).expect("first");
        guard.consume(owner, 2, T0 + 60, T0).expect("second");
        // Full of unexpired grants: a new one is refused, not an old one dropped.
        assert!(matches!(
            guard.consume(owner, 3, T0 + 60, T0),
            Err(ExchangeError::ReplayGuardFull { capacity: 2, .. })
        ));
        // Once grant 1 has expired it makes room; grant 2 is still a replay.
        guard
            .consume(owner, 3, T0 + 60, T0 + 11)
            .expect("room after expiry");
        assert!(matches!(
            guard.consume(owner, 2, T0 + 60, T0 + 11),
            Err(ExchangeError::AlreadyUsed { grant_id: 2, .. })
        ));
    }

    #[test]
    fn session_cookies_are_found_among_others() {
        let mut request = RequestHeader::build("GET", b"/", None).expect("request");
        request
            .append_header("cookie", "a=1; _temps_ingress=tok1")
            .expect("cookie");
        request
            .append_header("cookie", "_temps_ingress=\"tok2\"; b=2")
            .expect("cookie");
        let values: Vec<&str> = cookie_values(&request, SESSION_COOKIE_NAME).collect();
        assert_eq!(values, vec!["tok1", "tok2"]);
    }

    #[test]
    fn a_session_grant_admits_only_its_host_owner_and_permission() {
        let master = [9u8; 32];
        let owner = ProductRef {
            kind: ProductKind::Environment,
            id: 7,
        };
        let ep = endpoint(vec![AccessPolicy::TempsLogin {
            permission: "projects:read".into(),
        }]);
        let token = grant::mint(
            &GrantKey::derive(&master, owner),
            &grant::GrantClaims {
                grant_id: 3,
                usage: GrantUse::Session,
                owner,
                host: "app.example.test",
                permission: "projects:read",
                user_id: 1,
                expires_at: T0 + 60,
            },
        )
        .expect("mint");
        let mut request = RequestHeader::build("GET", b"/", None).expect("request");
        request
            .append_header("cookie", format!("{SESSION_COOKIE_NAME}={token}"))
            .expect("cookie");
        let none = Revocations::default();
        let revoked = Revocations::new([3]);
        let admits =
            |permission: &str, host: Option<&str>, now_secs: u64, revocations: &Revocations| {
                let facts = GrantFacts {
                    master: &master,
                    revocations,
                    host,
                    now_secs,
                };
                session_grant_admits(&request, &ep, permission, &facts)
            };
        let host = Some("app.example.test");
        assert!(admits("projects:read", host, T0, &none));
        assert!(!admits("projects:write", host, T0, &none));
        assert!(!admits(
            "projects:read",
            Some("other.example.test"),
            T0,
            &none
        ));
        assert!(!admits("projects:read", host, T0 + 61, &none));
        assert!(!admits("projects:read", host, T0, &revoked));
        assert!(!admits("projects:read", None, T0, &none));
    }

    #[test]
    fn grant_hosts_are_lowercase_without_port_or_dot() {
        let mut buffer = [0u8; 256];
        assert_eq!(
            grant_host("App.Example.Test:8443", &mut buffer),
            Some("app.example.test")
        );
        assert_eq!(
            grant_host("app.example.test.", &mut buffer),
            Some("app.example.test")
        );
        let long = "a".repeat(300);
        assert_eq!(grant_host(&long, &mut buffer), None);
    }

    /// The Phase 8 differential proof: every request in
    /// `testdata/ingress_differential_corpus.json` gets the same response
    /// from the legacy chain (with the proxy's real IP gate and rate
    /// limiter) as from the compiled endpoint the route snapshot builds for
    /// the same settings, both enforcing (its own limiter) and in shadow
    /// (replaying the legacy chain's facts).
    mod differential {
        use super::*;
        use crate::ip_access::ProjectIpAccessGate;
        use serde::Deserialize;
        use std::collections::HashMap;
        use temps_core::RateLimitSettings;
        use temps_entities::deployment_config::{
            DeploymentConfig, PasswordProtectionConfig, SecurityConfig,
        };
        use temps_entities::project_ip_access_rules::{IpAccessCidrEntry, IpAccessCidrs};
        use temps_entities::{environments, project_ip_access_rules, projects};
        use temps_routes::ingress::{environment_endpoint, EnvironmentAccess, IngressSnapshot};

        const CORPUS: &str = include_str!("../testdata/ingress_differential_corpus.json");
        const PROJECT_ID: i32 = 3;
        const ENVIRONMENT_ID: i32 = 7;
        const OTHER_ENVIRONMENT_ID: i32 = 8;

        #[derive(Deserialize)]
        struct Corpus {
            scenarios: Vec<Scenario>,
        }

        #[derive(Deserialize)]
        struct Scenario {
            name: String,
            host: String,
            #[serde(default)]
            instance: Option<RateLimitSettings>,
            #[serde(default)]
            project_attack_mode: bool,
            #[serde(default)]
            environment_attack_mode: Option<bool>,
            #[serde(default)]
            project_security: Option<SecurityConfig>,
            #[serde(default)]
            environment_security: Option<SecurityConfig>,
            #[serde(default)]
            password: bool,
            #[serde(default)]
            ip_rules: Vec<IpRuleRow>,
            requests: Vec<Request>,
        }

        #[derive(Deserialize)]
        struct IpRuleRow {
            #[serde(default)]
            environment: bool,
            #[serde(default)]
            other_environment: bool,
            mode: String,
            cidrs: Vec<String>,
        }

        fn slash() -> String {
            "/".into()
        }
        fn get() -> String {
            "GET".into()
        }
        fn yes() -> bool {
            true
        }
        fn one() -> u32 {
            1
        }

        #[derive(Deserialize)]
        struct Request {
            ip: Option<String>,
            #[serde(default = "slash")]
            path: String,
            #[serde(default = "get")]
            method: String,
            #[serde(default = "yes")]
            tls: bool,
            #[serde(default)]
            captcha_completed: bool,
            #[serde(default)]
            password_cookie: bool,
            #[serde(default = "one")]
            repeat: u32,
            #[serde(default)]
            at: u64,
        }

        fn project(scenario: &Scenario) -> projects::Model {
            let now = chrono::Utc::now();
            let mut project: projects::Model = serde_json::from_value(serde_json::json!({
                "id": PROJECT_ID, "name": "shop", "slug": "shop", "repo_name": "shop",
                "repo_owner": "example", "directory": ".", "main_branch": "main", "preset": "dockerfile",
                "created_at": now, "updated_at": now, "pull_only_root_directory": false,
                "is_deleted": false, "is_public_repo": false, "attack_mode": false,
                "ai_write_actions_enabled": false, "error_source_context_enabled": false,
                "vulnerability_scanning_enabled": false, "enable_preview_environments": false,
                "preview_envs_on_demand": false, "preview_envs_idle_timeout_seconds": 300,
                "preview_envs_wake_timeout_seconds": 30, "source_type": "docker_image",
                "project_type": temps_entities::types::ProjectType::Server,
                "hosting": temps_entities::types::Hosting::Hosted,
                "cross_project_trace_sharing": true, "cloud_telemetry_fidelity": "metered",
                "cloud_telemetry_attribute_allowlist": [], "cloud_telemetry_write_mode": "local",
                "cloud_analytics_write_mode": "local"
            }))
            .expect("project fixture");
            project.attack_mode = scenario.project_attack_mode;
            project.deployment_config =
                scenario
                    .project_security
                    .clone()
                    .map(|security| DeploymentConfig {
                        security: Some(security),
                        ..DeploymentConfig::default()
                    });
            project
        }

        fn environment(scenario: &Scenario) -> environments::Model {
            let mut environment: environments::Model = serde_json::from_value(serde_json::json!({
                "id": ENVIRONMENT_ID, "name": "production", "slug": "production",
                "subdomain": "app", "last_deployment": null, "host": scenario.host,
                "upstreams": [], "created_at": "2026-09-28T00:00:00Z",
                "updated_at": "2026-09-28T00:00:00Z", "project_id": PROJECT_ID,
                "current_deployment_id": null, "branch": null, "deleted_at": null,
                "deployment_config": null, "is_preview": false, "protected": false,
                "sleeping": false, "attack_mode": null, "force_https": null,
                "last_activity_at": null, "ingress_mode": "enforce"
            }))
            .expect("environment fixture");
            environment.attack_mode = scenario.environment_attack_mode;
            let mut security = scenario.environment_security.clone();
            if scenario.password {
                security
                    .get_or_insert_with(SecurityConfig::default)
                    .password_protection = Some(PasswordProtectionConfig {
                    enabled: true,
                    password_hash: "corpus-password-hash".into(),
                });
            }
            environment.deployment_config = security.map(|security| DeploymentConfig {
                security: Some(security),
                ..DeploymentConfig::default()
            });
            environment
        }

        fn rules(scenario: &Scenario) -> Vec<project_ip_access_rules::Model> {
            let now = chrono::Utc::now();
            scenario
                .ip_rules
                .iter()
                .enumerate()
                .map(|(index, row)| project_ip_access_rules::Model {
                    id: index as i32 + 1,
                    project_id: PROJECT_ID,
                    environment_id: if row.environment {
                        Some(ENVIRONMENT_ID)
                    } else if row.other_environment {
                        Some(OTHER_ENVIRONMENT_ID)
                    } else {
                        None
                    },
                    mode: row.mode.clone(),
                    cidrs: IpAccessCidrs(
                        row.cidrs
                            .iter()
                            .map(|cidr| IpAccessCidrEntry {
                                cidr: cidr.clone(),
                                description: None,
                            })
                            .collect(),
                    ),
                    updated_by: None,
                    revision: 1,
                    created_at: now,
                    updated_at: now,
                })
                .collect()
        }

        /// What the proxy's `decide_environment_access` does with a step,
        /// with the captcha cache's answer given.
        fn finish_enforce(
            endpoint: &CompiledEndpoint,
            request: &EndpointRequest<'_>,
            limiter: &RateLimiter,
            captcha_completed: bool,
        ) -> AccessOutcome {
            match enforce_step(endpoint, request, limiter, false, 0) {
                Step::Done(outcome) => outcome,
                Step::NeedsCaptcha { resume_at } if captcha_completed => {
                    match enforce_step(endpoint, request, limiter, true, resume_at) {
                        Step::Done(outcome) => outcome,
                        Step::NeedsCaptcha { .. } => AccessOutcome::CaptchaChallenge,
                    }
                }
                Step::NeedsCaptcha { .. } => AccessOutcome::CaptchaChallenge,
            }
        }

        #[test]
        fn legacy_and_compiled_endpoints_decide_every_corpus_request_alike() {
            let corpus: Corpus = serde_json::from_str(CORPUS).expect("corpus parses");
            let master = [5u8; 32];
            let mut requests = 0usize;
            let mut outcomes: HashMap<&'static str, usize> = HashMap::new();
            let mut mismatches = Vec::new();

            for scenario in &corpus.scenarios {
                let project = project(scenario);
                let environment = environment(scenario);
                let rows = rules(scenario);
                let instance = scenario.instance.clone().unwrap_or_default();

                // Legacy: the proxy's own gate and limiter.
                let gate = ProjectIpAccessGate::from_rows_for_tests(rows.clone());
                let legacy_limiter = RateLimiter::new();
                let effective = environment.get_effective_deployment_config(
                    &project.deployment_config.clone().unwrap_or_default(),
                );
                let rate_limit = crate::rate_limiter::resolve_policy(
                    &instance,
                    effective.security.as_ref(),
                    ENVIRONMENT_ID,
                );

                // Compiled: what the route snapshot builds for these settings.
                let access = EnvironmentAccess {
                    environment: &environment,
                    project: &project,
                    instance_rate_limiting: &instance,
                    ip_rules: &rows,
                };
                let snapshot = IngressSnapshot::build(
                    vec![environment_endpoint(1, &access, None, Vec::new())],
                    HashMap::new(),
                    Revocations::default(),
                );
                assert!(
                    !snapshot.is_broken(ENVIRONMENT_ID),
                    "scenario '{}': derived endpoint must compile",
                    scenario.name
                );
                let endpoint = snapshot
                    .environment_endpoint(ENVIRONMENT_ID, "/")
                    .unwrap_or_else(|| panic!("scenario '{}': no endpoint", scenario.name));
                let enforce_limiter = RateLimiter::new();
                let header = RequestHeader::build("GET", b"/", None).expect("request header");

                for (index, request) in scenario.requests.iter().enumerate() {
                    let client_ip = request
                        .ip
                        .as_deref()
                        .and_then(|ip| ip.parse::<IpAddr>().ok())
                        .map(crate::proxy::normalize_client_ip);
                    let now_secs = T0 + request.at;
                    let password = scenario.password.then_some(PasswordFacts {
                        verify_post: request.path
                            == crate::handler::password_wall::PASSWORD_VERIFY_PATH
                            && request.method == "POST",
                        cookie_valid: request.password_cookie,
                    });
                    let captcha_path = request.path.starts_with("/api/_temps/captcha")
                        || request.path.starts_with("/api/__temps/temps_captcha_wasm");
                    for repeat in 0..request.repeat {
                        requests += 1;
                        let legacy = LegacyRequest {
                            client_ip,
                            ip_restricted: crate::proxy::legacy_ip_gate_denies(
                                temps_core::RequestPolicyDecision::Continue,
                                &gate,
                                PROJECT_ID,
                                ENVIRONMENT_ID,
                                client_ip,
                            ),
                            rate_limit,
                            attack_mode: environment.attack_mode.unwrap_or(project.attack_mode),
                            captcha_path,
                            has_tls_fingerprint: request.tls,
                            password,
                        };
                        let mut facts = LegacyFacts::default();
                        let legacy_outcome =
                            match legacy_access(&legacy, &legacy_limiter, now_secs, &mut facts) {
                                Step::Done(outcome) => outcome,
                                Step::NeedsCaptcha { .. } => legacy_after_captcha(
                                    &legacy,
                                    request.captcha_completed,
                                    &mut facts,
                                ),
                            };

                        let compiled = EndpointRequest {
                            request: &header,
                            environment_id: ENVIRONMENT_ID,
                            client_ip,
                            captcha_path,
                            has_tls_fingerprint: request.tls,
                            password,
                            grants: GrantFacts {
                                master: &master,
                                revocations: snapshot.revocations(),
                                host: Some(scenario.host.as_str()),
                                now_secs,
                            },
                        };
                        let enforced = finish_enforce(
                            endpoint,
                            &compiled,
                            &enforce_limiter,
                            request.captcha_completed,
                        );
                        let shadow = shadow_step(endpoint, &compiled, &facts);

                        *outcomes.entry(legacy_outcome.label()).or_default() += 1;
                        if enforced.response_class() != legacy_outcome.response_class()
                            || shadow.response_class() != legacy_outcome.response_class()
                        {
                            mismatches.push(format!(
                                "scenario '{}', request {} (repeat {}): legacy {:?}, enforce {:?}, shadow {:?}",
                                scenario.name, index, repeat, legacy_outcome, enforced, shadow
                            ));
                        }
                    }
                }
            }

            assert!(
                mismatches.is_empty(),
                "{} of {} corpus requests decided differently:\n{}",
                mismatches.len(),
                requests,
                mismatches.join("\n")
            );
            // The corpus must exercise every legacy outcome it can produce.
            for label in [
                "allow",
                "ip_forbidden",
                "rate_limit_blacklisted",
                "too_many_requests",
                "https_required",
                "captcha_challenge",
                "password_wall",
            ] {
                assert!(
                    outcomes.get(label).copied().unwrap_or(0) > 0,
                    "corpus never produces '{label}'"
                );
            }
            println!(
                "ingress differential corpus: {} scenarios, {} requests, 0 mismatches ({:?})",
                corpus.scenarios.len(),
                requests,
                outcomes
            );
        }
    }
}
