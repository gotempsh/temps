// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One decision per request, the same on every ingress (ADR 066, D1, D4,
//! D6, D7 and D8).
//!
//! Both proxies, the control plane's and a worker's, describe the request
//! as a [`Request`], call [`ShieldEngine::decide`], and turn the
//! [`Outcome`] into a response. Everything between the two is here, so the
//! two ingresses cannot drift apart.

use std::net::IpAddr;
use std::sync::Arc;

use serde::Deserialize;

use crate::browsers;
use crate::crawlers::{self, Crawler};
use crate::difficulty::{self, ChallengeCause, FailureTracker, LoadTracker};
use crate::gate::{Admission, ChallengeGate};
use crate::identity::{VisitorIdentity, VisitorNetwork};
use crate::index::Index;
use crate::policy::{EnvPolicy, ShieldMode};
use crate::token::ChallengeError;
use crate::verdict::{Action, Reason, Subject, Verdict};

/// A request as Shield needs to see it. Every field is what the ingress
/// resolved from the connection, never what a body claims.
#[derive(Debug, Clone, Copy, Default)]
pub struct Request<'a> {
    /// The client address, as the ingress resolved it.
    pub ip: Option<IpAddr>,
    /// The client's JA4, computed or taken from a trusted forwarder.
    pub ja4: Option<&'a str>,
    /// How the client reached the ingress (`direct`, `cloudflare`, …),
    /// which stands in for the JA4 in the visitor identity when there is
    /// none (D3).
    pub source: &'a str,
    pub user_agent: &'a str,
    pub method: &'a str,
    /// The path alone, as routing and exemptions read it.
    pub path: &'a str,
    /// The path with its query, as sent.
    pub path_and_query: &'a str,
    pub accept: &'a str,
    /// Request headers; only the ones exploit signatures read are used.
    pub headers: &'a [(&'a str, &'a str)],
    /// The value of this environment's clearance cookie.
    pub clearance: Option<&'a str>,
    /// The value of the bypass token header.
    pub bypass_token: Option<&'a str>,
    /// Whether the request came over TLS, without which the clearance
    /// cookie (`__Host-`, `Secure`) cannot be set.
    pub tls: bool,
}

impl Request<'_> {
    /// A page load: the only kind of request a challenge page is served to
    /// (D7).
    pub fn is_navigation(&self) -> bool {
        (self.method.eq_ignore_ascii_case("GET") || self.method.eq_ignore_ascii_case("HEAD"))
            && self
                .accept
                .split(',')
                .any(|part| part.trim().starts_with("text/html"))
    }

    fn identity(&self) -> VisitorIdentity {
        match self.ja4 {
            Some(ja4) => VisitorIdentity::compute(self.ip, self.user_agent, ja4),
            None => {
                VisitorIdentity::compute(self.ip, self.user_agent, &format!("none:{}", self.source))
            }
        }
    }
}

/// What the ingress does with the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Serve it.
    Pass,
    /// 403.
    Block,
    /// Serve the challenge page for `token`.
    Challenge { token: String, difficulty: u8 },
    /// A challenge for a request that cannot show a page (D7): 429 with
    /// `Retry-After` and a problem body.
    TooManyRequests,
    /// A challenge over plain HTTP, where no clearance can be set: 426.
    HttpsRequired,
    /// The challenge could not be issued: 503.
    Unavailable,
}

impl Outcome {
    /// Whether Shield answered the request itself.
    pub fn answers(&self) -> bool {
        !matches!(self, Self::Pass)
    }
}

/// Why a request that had a verdict was served anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admitted {
    /// `log` mode: verdicts are recorded, never acted on.
    LogOnly,
    BypassToken(i64),
    ExemptPath,
    Clearance,
    VerifiedCrawler,
}

impl Admitted {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LogOnly => "log_only",
            Self::BypassToken(_) => "bypass_token",
            Self::ExemptPath => "exempt_path",
            Self::Clearance => "clearance",
            Self::VerifiedCrawler => "verified_crawler",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub mode: ShieldMode,
    /// What the lists and signals said; recorded with the request log and
    /// as an event when it is not `pass`.
    pub verdict: Verdict,
    pub outcome: Outcome,
    /// Set when a verdict other than `pass` did not stop the request.
    pub admitted: Option<Admitted>,
    /// The crawler the User-Agent claims, set when the request would be
    /// challenged in any form (page, 429, 426 or 503): the ingress
    /// verifies the address and passes a verified one instead (D7).
    pub crawler: Option<&'static Crawler>,
}

impl Decision {
    fn pass(mode: ShieldMode) -> Self {
        Self {
            mode,
            verdict: Verdict::PASS,
            outcome: Outcome::Pass,
            admitted: None,
            crawler: None,
        }
    }

    /// Whether this decision is an event (D9, D11): anything but a plain
    /// pass.
    pub fn is_event(&self) -> bool {
        self.verdict.action != Action::Pass || self.outcome.answers()
    }

    /// What `X-Temps-Shield` says about it (D11), when Shield acted.
    pub fn header_verdict(&self) -> Option<&'static str> {
        match self.outcome {
            Outcome::Pass => None,
            Outcome::Block => Some("block"),
            Outcome::Challenge { .. } | Outcome::TooManyRequests | Outcome::HttpsRequired => {
                Some("challenge")
            }
            Outcome::Unavailable => Some("error"),
        }
    }

    /// The difficulty of the challenge served, if one was.
    pub fn difficulty(&self) -> Option<u8> {
        match self.outcome {
            Outcome::Challenge { difficulty, .. } => Some(difficulty),
            _ => None,
        }
    }
}

/// The `X-Temps-Shield` response header (D11): the verdict and the request
/// id, never the reason.
pub const RESPONSE_HEADER: &str = "x-temps-shield";

pub fn response_header_value(verdict: &str, request_id: &str) -> String {
    format!("{verdict}; ref={request_id}")
}

#[derive(Debug, Deserialize)]
struct VerifyBody {
    token: String,
    nonce: String,
    /// How long the browser took, for the difficulty measurements of D6.
    #[serde(default)]
    solve_ms: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The solution holds: store this `Set-Cookie` value, the clearance for
    /// `env_id`.
    Cleared {
        env_id: i32,
        set_cookie: String,
        solve_ms: Option<u64>,
    },
    /// A well-formed request whose token or solution does not hold.
    Rejected(ChallengeError),
    /// Not a verify request body at all.
    BadRequest,
}

/// The status and short reason a rejected verify returns to the page. A
/// full redeemed set is the ingress's limit and a full network share is a
/// rate limit, not a bad solution; the page offers a retry for every one.
pub fn rejection_response(error: ChallengeError) -> (u16, &'static str) {
    match error {
        ChallengeError::Busy => (503, "busy"),
        ChallengeError::NetworkBusy => (429, "rate_limited"),
        ChallengeError::Expired => (403, "expired"),
        ChallengeError::Replayed => (403, "replayed"),
        ChallengeError::InsufficientWork | ChallengeError::BadNonce => (403, "unsolved"),
        ChallengeError::Malformed
        | ChallengeError::BadSignature
        | ChallengeError::WrongVisitor
        | ChallengeError::Key => (403, "invalid"),
    }
}

/// Shield on one ingress: the challenge gate and what it measures.
pub struct ShieldEngine {
    gate: Arc<ChallengeGate>,
    load: LoadTracker,
    failures: FailureTracker,
}

impl ShieldEngine {
    pub fn new(gate: Arc<ChallengeGate>) -> Self {
        Self {
            gate,
            load: LoadTracker::default(),
            failures: FailureTracker::default(),
        }
    }

    pub fn gate(&self) -> &ChallengeGate {
        &self.gate
    }

    /// What to do with `request` for the environment `policy` describes.
    ///
    /// `index` is the loaded snapshot, `local` what this node adds on its
    /// own (netwatch's bans). No I/O; the pass path allocates nothing
    /// unless a list or signature matches.
    pub fn decide(
        &self,
        index: &Index,
        local: Option<&Index>,
        policy: &EnvPolicy,
        request: &Request<'_>,
        now: u64,
    ) -> Decision {
        let mode = policy.mode;
        if mode == ShieldMode::Off {
            return Decision::pass(mode);
        }
        self.load.record_request(policy.environment_id, now);
        let verdict = evaluate(index, local, policy.project_id, request, now as i64);
        let mut decision = Decision {
            mode,
            verdict,
            outcome: Outcome::Pass,
            admitted: None,
            crawler: None,
        };
        let listed = decision.verdict.action;
        if !mode.enforces() {
            if listed != Action::Pass {
                decision.admitted = Some(Admitted::LogOnly);
            }
            return decision;
        }
        let needs_challenge = listed == Action::Challenge || mode == ShieldMode::Attack;
        if listed != Action::Block && !needs_challenge {
            return decision;
        }
        if let Some(id) = request
            .bypass_token
            .and_then(|token| policy.bypass.check(token))
        {
            decision.admitted = Some(Admitted::BypassToken(id));
            return decision;
        }
        if listed == Action::Block {
            decision.outcome = Outcome::Block;
            return decision;
        }
        if policy.exempt_paths.matches(request.path) {
            decision.admitted = Some(Admitted::ExemptPath);
            return decision;
        }
        let identity = request.identity();
        let env_id = policy.environment_id;
        let admission = request
            .clearance
            .map(|cookie| self.gate.admit(cookie, env_id, &identity, now))
            .unwrap_or(Admission::NotCleared);
        let cause = match admission {
            Admission::Admitted => {
                decision.admitted = Some(Admitted::Clearance);
                return decision;
            }
            Admission::BudgetSpent => ChallengeCause::BudgetSpent,
            Admission::NotCleared if listed == Action::Challenge => ChallengeCause::Verdict,
            Admission::NotCleared => ChallengeCause::Attack,
        };
        // Every challenge below, page or not, is lifted for a verified
        // crawler: a 429 on robots.txt would stop it crawling.
        decision.crawler = crawlers::claimed(request.user_agent);
        if !request.is_navigation() {
            decision.outcome = Outcome::TooManyRequests;
            return decision;
        }
        if !request.tls {
            decision.outcome = Outcome::HttpsRequired;
            return decision;
        }
        let difficulty = difficulty::difficulty(
            cause,
            self.failures.repeated(&identity, now),
            self.load.extra_bits(env_id, now),
        );
        decision.outcome = match self.gate.issue(env_id, &identity, difficulty, now) {
            Ok(token) => Outcome::Challenge { token, difficulty },
            Err(_) => Outcome::Unavailable,
        };
        decision
    }

    /// Turns a challenge, in any form, for a crawler whose address the
    /// ingress verified into a pass (D7).
    pub fn admit_verified_crawler(decision: &mut Decision) {
        if decision.crawler.is_some()
            && matches!(
                decision.outcome,
                Outcome::Challenge { .. }
                    | Outcome::TooManyRequests
                    | Outcome::HttpsRequired
                    | Outcome::Unavailable
            )
        {
            decision.outcome = Outcome::Pass;
            decision.admitted = Some(Admitted::VerifiedCrawler);
        }
    }

    /// Counts a response's status for the adaptive difficulty (D6).
    pub fn record_status(&self, env_id: i32, status: u16, now: u64) {
        self.load.record_status(env_id, status, now);
    }

    /// Verifies a posted solution for the visitor the ingress resolved for
    /// this request. The clearance is for the environment the signed token
    /// names, whichever environment the verify path itself resolved to.
    pub fn verify(&self, body: &[u8], request: &Request<'_>, now: u64) -> VerifyOutcome {
        let Ok(body) = serde_json::from_slice::<VerifyBody>(body) else {
            return VerifyOutcome::BadRequest;
        };
        let identity = request.identity();
        let network = VisitorNetwork::of(request.ip);
        match self
            .gate
            .redeem(&body.token, &body.nonce, &identity, network, now)
        {
            Ok(clearance) => VerifyOutcome::Cleared {
                env_id: clearance.env_id,
                set_cookie: clearance.set_cookie(),
                solve_ms: body.solve_ms,
            },
            Err(error) => {
                if matches!(
                    error,
                    ChallengeError::InsufficientWork
                        | ChallengeError::BadNonce
                        | ChallengeError::WrongVisitor
                        | ChallengeError::BadSignature
                        | ChallengeError::Malformed
                ) {
                    self.failures.record(&identity, now);
                }
                VerifyOutcome::Rejected(error)
            }
        }
    }
}

/// What the lists, the exploit signatures and the fingerprint say about a
/// request (D2, D4, D8).
pub fn evaluate(
    index: &Index,
    local: Option<&Index>,
    project_id: i32,
    request: &Request<'_>,
    now: i64,
) -> Verdict {
    let lookup = index.evaluate_client(local, request.ip, request.ja4, project_id, now);
    // An allow rule passes the client before anything it sends is looked at.
    if lookup.allowed {
        return lookup.verdict;
    }
    let mut verdict = lookup.verdict;
    if let Some(signature) =
        temps_signatures::match_parts(request.method, request.path_and_query, request.headers)
    {
        // A probe for leaked secrets is not an exploit: it blocks only from
        // an address the lists already distrust (D8).
        let action = if !signature.recon() || verdict.action >= Action::Challenge {
            Action::Block
        } else {
            Action::Log
        };
        verdict.add(
            action,
            Reason {
                source: Arc::from("signatures"),
                subject: Subject::Signature {
                    id: signature.id,
                    cve: signature.cve,
                    path: Arc::from(request.path),
                },
                stale: false,
            },
        );
    }
    if let Some((claimed, tool)) = browsers::mismatch(request.user_agent, request.ja4) {
        verdict.add(
            Action::Challenge,
            Reason {
                source: Arc::from("client_mismatch"),
                subject: Subject::ClientMismatch { claimed, tool },
                stale: false,
            },
        );
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bypass;
    use crate::keys::ShieldKeys;
    use crate::policy::ExemptPaths;
    use crate::snapshot::{ScopedList, ShieldSnapshot, SourceList};
    use crate::token::has_leading_zero_bits;
    use crate::verdict::ListVerdict;
    use crate::Network;
    use sha2::{Digest, Sha256};

    const NOW: u64 = 1_800_000_000;
    const LISTED_BLOCK: &str = "185.93.89.7";
    const LISTED_CHALLENGE: &str = "45.33.32.9";
    const CLEAN: &str = "8.8.8.8";
    const CHROME: &str = "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/141.0 Safari/537.36";
    const CHROME_JA4: &str = "t13d1516h2_8daaf6152771_02713d6af862";

    fn engine() -> ShieldEngine {
        ShieldEngine::new(Arc::new(ChallengeGate::new(ShieldKeys::derive(|label| {
            Sha256::digest(label.as_bytes()).into()
        }))))
    }

    fn index() -> Index {
        let list = |source: &str, verdict, net: &str| {
            SourceList::new(
                source,
                verdict,
                NOW as i64,
                None,
                [Network::parse(net).unwrap()],
            )
        };
        Index::new(&ShieldSnapshot {
            revision: 1,
            generated_at: NOW as i64,
            lists: vec![
                list("spamhaus_drop", ListVerdict::Block, "185.93.89.0/24"),
                list("dshield", ListVerdict::Challenge, "45.33.32.0/24"),
            ],
            scoped: vec![ScopedList {
                project_id: 1,
                list: list("operator", ListVerdict::Allow, "185.93.89.8"),
            }],
            ..ShieldSnapshot::default()
        })
    }

    fn policy(mode: ShieldMode) -> EnvPolicy {
        EnvPolicy {
            project_id: 1,
            environment_id: 10,
            mode,
            exempt_paths: ExemptPaths::parse(&["/api/*"]).unwrap(),
            bypass: Default::default(),
        }
    }

    fn page(ip: &str) -> Request<'static> {
        Request {
            ip: Some(ip.parse().unwrap()),
            ja4: Some(CHROME_JA4),
            source: "direct",
            user_agent: CHROME,
            method: "GET",
            path: "/",
            path_and_query: "/",
            accept: "text/html,application/xhtml+xml,*/*;q=0.8",
            tls: true,
            ..Request::default()
        }
    }

    fn decide(engine: &ShieldEngine, mode: ShieldMode, request: &Request<'_>) -> Decision {
        engine.decide(&index(), None, &policy(mode), request, NOW)
    }

    fn solve(token: &str, difficulty: u8) -> String {
        (0u64..)
            .map(|nonce| nonce.to_string())
            .find(|nonce| {
                let hash = Sha256::digest(format!("{token}{nonce}").as_bytes());
                has_leading_zero_bits(&hash, u32::from(difficulty))
            })
            .unwrap()
    }

    #[test]
    fn each_mode_acts_on_verdicts_as_d1_says() {
        let engine = engine();
        // off: nothing, not even a record.
        let off = decide(&engine, ShieldMode::Off, &page(LISTED_BLOCK));
        assert_eq!(off.outcome, Outcome::Pass);
        assert!(!off.is_event());
        // log: recorded, served.
        let log = decide(&engine, ShieldMode::Log, &page(LISTED_BLOCK));
        assert_eq!(log.outcome, Outcome::Pass);
        assert_eq!(log.verdict.action, Action::Block);
        assert_eq!(log.admitted, Some(Admitted::LogOnly));
        assert!(log.is_event() && log.header_verdict().is_none());
        // selective: block and challenge, clean visitors pass.
        let block = decide(&engine, ShieldMode::Selective, &page(LISTED_BLOCK));
        assert_eq!(block.outcome, Outcome::Block);
        assert_eq!(block.header_verdict(), Some("block"));
        let challenge = decide(&engine, ShieldMode::Selective, &page(LISTED_CHALLENGE));
        assert!(matches!(
            challenge.outcome,
            Outcome::Challenge { difficulty: 18, .. }
        ));
        assert_eq!(challenge.crawler, None);
        let clean = decide(&engine, ShieldMode::Selective, &page(CLEAN));
        assert_eq!(clean.outcome, Outcome::Pass);
        assert!(!clean.is_event());
        // attack: everyone else is challenged, harder.
        let attack = decide(&engine, ShieldMode::Attack, &page(CLEAN));
        assert!(matches!(
            attack.outcome,
            Outcome::Challenge { difficulty: 20, .. }
        ));
        assert_eq!(attack.verdict.action, Action::Pass);
        assert!(attack.is_event());
    }

    #[test]
    fn only_page_loads_over_tls_get_a_page() {
        let engine = engine();
        let mut api = page(LISTED_CHALLENGE);
        api.accept = "application/json";
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &api).outcome,
            Outcome::TooManyRequests
        );
        let mut post = page(LISTED_CHALLENGE);
        post.method = "POST";
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &post).outcome,
            Outcome::TooManyRequests
        );
        let mut plain = page(LISTED_CHALLENGE);
        plain.tls = false;
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &plain).outcome,
            Outcome::HttpsRequired
        );
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &plain).header_verdict(),
            Some("challenge")
        );
    }

    #[test]
    fn exemptions_skip_challenges_and_only_a_bypass_token_skips_a_block() {
        let engine = engine();
        let mut exempt = page(LISTED_CHALLENGE);
        exempt.path = "/api/users";
        let decision = decide(&engine, ShieldMode::Selective, &exempt);
        assert_eq!(
            (decision.outcome, decision.admitted),
            (Outcome::Pass, Some(Admitted::ExemptPath))
        );
        let mut exempt_block = page(LISTED_BLOCK);
        exempt_block.path = "/api/users";
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &exempt_block).outcome,
            Outcome::Block
        );

        let (token, hash) = bypass::generate(5);
        let mut with_token = policy(ShieldMode::Selective);
        with_token.bypass = bypass::TokenSet::new([hash]);
        let mut monitor = page(LISTED_BLOCK);
        monitor.bypass_token = Some(&token);
        let decision = engine.decide(&index(), None, &with_token, &monitor, NOW);
        assert_eq!(decision.outcome, Outcome::Pass);
        assert_eq!(decision.admitted, Some(Admitted::BypassToken(5)));
        // Another environment's token is just a header.
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &monitor).outcome,
            Outcome::Block
        );
    }

    #[test]
    fn an_operator_allow_rule_wins_over_lists_and_signatures() {
        let engine = engine();
        let mut allowed = page("185.93.89.8");
        allowed.path = "/.git/config";
        allowed.path_and_query = "/.git/config";
        let decision = decide(&engine, ShieldMode::Attack, &allowed);
        assert_eq!(decision.verdict, Verdict::PASS);
        // Attack mode still challenges everyone without a clearance.
        assert!(matches!(decision.outcome, Outcome::Challenge { .. }));
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &allowed).outcome,
            Outcome::Pass
        );
    }

    #[test]
    fn a_clearance_admits_within_its_budget_then_challenges_harder() {
        let engine = ShieldEngine::new(Arc::new(ChallengeGate::with_budget(
            ShieldKeys::derive(|label| Sha256::digest(label.as_bytes()).into()),
            2,
        )));
        let request = page(LISTED_CHALLENGE);
        let Outcome::Challenge {
            token, difficulty, ..
        } = decide(&engine, ShieldMode::Selective, &request).outcome
        else {
            panic!("expected a challenge");
        };
        let body = format!(
            r#"{{"token":"{token}","nonce":"{}","solve_ms":900}}"#,
            solve(&token, difficulty)
        );
        let VerifyOutcome::Cleared {
            env_id,
            set_cookie,
            solve_ms,
        } = engine.verify(body.as_bytes(), &request, NOW + 1)
        else {
            panic!("expected a clearance");
        };
        assert_eq!((env_id, solve_ms), (10, Some(900)));
        let cookie = set_cookie
            .split_once('=')
            .and_then(|(_, rest)| rest.split(';').next())
            .unwrap()
            .to_string();
        let mut cleared = page(LISTED_CHALLENGE);
        cleared.clearance = Some(&cookie);
        for _ in 0..2 {
            let decision = decide(&engine, ShieldMode::Selective, &cleared);
            assert_eq!(decision.admitted, Some(Admitted::Clearance));
        }
        let spent = decide(&engine, ShieldMode::Selective, &cleared);
        assert!(matches!(
            spent.outcome,
            Outcome::Challenge { difficulty: 20, .. }
        ));
        // A clearance never admits a block.
        let mut blocked = page(LISTED_BLOCK);
        blocked.clearance = Some(&cookie);
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &blocked).outcome,
            Outcome::Block
        );
    }

    #[test]
    fn repeated_failures_raise_the_difficulty() {
        let engine = engine();
        let request = page(LISTED_CHALLENGE);
        let Outcome::Challenge { token, .. } =
            decide(&engine, ShieldMode::Selective, &request).outcome
        else {
            panic!("expected a challenge");
        };
        let wrong = format!(r#"{{"token":"{token}","nonce":"x"}}"#);
        for _ in 0..difficulty::REPEATED_FAILURES {
            assert!(matches!(
                engine.verify(wrong.as_bytes(), &request, NOW),
                VerifyOutcome::Rejected(_)
            ));
        }
        assert_eq!(
            engine.verify(b"{}", &request, NOW),
            VerifyOutcome::BadRequest
        );
        assert!(matches!(
            decide(&engine, ShieldMode::Selective, &request).outcome,
            Outcome::Challenge { difficulty: 22, .. }
        ));
    }

    #[test]
    fn exploits_block_and_probes_block_only_from_distrusted_addresses() {
        let engine = engine();
        let mut exploit = page(CLEAN);
        exploit.path = "/cgi-bin/.%2e/.%2e/bin/sh";
        exploit.path_and_query = "/cgi-bin/.%2e/.%2e/bin/sh";
        let decision = decide(&engine, ShieldMode::Selective, &exploit);
        assert_eq!(decision.outcome, Outcome::Block);
        assert_eq!(decision.verdict.signature(), Some("CVE-2021-41773"));
        assert_eq!(
            decide(&engine, ShieldMode::Log, &exploit).outcome,
            Outcome::Pass
        );

        let probe = |ip| {
            let mut request = page(ip);
            request.path = "/.env";
            request.path_and_query = "/.env";
            request
        };
        let from_clean = decide(&engine, ShieldMode::Selective, &probe(CLEAN));
        assert_eq!(from_clean.outcome, Outcome::Pass);
        assert_eq!(from_clean.verdict.action, Action::Log);
        assert!(from_clean.is_event(), "a probe still counts (D9)");
        assert_eq!(
            decide(&engine, ShieldMode::Selective, &probe(LISTED_CHALLENGE)).outcome,
            Outcome::Block
        );
    }

    #[test]
    fn a_browser_user_agent_from_a_tool_is_challenged() {
        let engine = engine();
        let mut curl = page(CLEAN);
        curl.ja4 = Some("t13d3012h2_1d37bd780c83_882d495ac381");
        let decision = decide(&engine, ShieldMode::Selective, &curl);
        assert!(matches!(decision.outcome, Outcome::Challenge { .. }));
        assert_eq!(
            decision.verdict.reason_strings(),
            ["client_mismatch (claims Chrome, ja4 of curl)"]
        );
    }

    #[test]
    fn a_claimed_crawler_is_named_for_the_ingress_to_verify() {
        let engine = engine();
        let mut googlebot = page(LISTED_CHALLENGE);
        googlebot.user_agent =
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
        let mut decision = decide(&engine, ShieldMode::Selective, &googlebot);
        assert!(matches!(decision.outcome, Outcome::Challenge { .. }));
        assert_eq!(decision.crawler.map(|c| c.name), Some("Googlebot"));
        ShieldEngine::admit_verified_crawler(&mut decision);
        assert_eq!(decision.outcome, Outcome::Pass);
        assert_eq!(decision.admitted, Some(Admitted::VerifiedCrawler));
    }

    #[test]
    fn a_crawler_fetching_robots_txt_in_attack_mode_is_named_too() {
        let engine = engine();
        let mut robots = page(CLEAN);
        robots.path = "/robots.txt";
        robots.path_and_query = "/robots.txt";
        robots.accept = "*/*";
        robots.user_agent =
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
        let mut decision = decide(&engine, ShieldMode::Attack, &robots);
        assert_eq!(decision.outcome, Outcome::TooManyRequests);
        assert_eq!(decision.crawler.map(|c| c.name), Some("Googlebot"));
        ShieldEngine::admit_verified_crawler(&mut decision);
        assert_eq!(decision.outcome, Outcome::Pass);
        assert_eq!(decision.admitted, Some(Admitted::VerifiedCrawler));

        let mut http = page(CLEAN);
        http.tls = false;
        http.user_agent = robots.user_agent;
        let decision = decide(&engine, ShieldMode::Attack, &http);
        assert_eq!(decision.outcome, Outcome::HttpsRequired);
        assert!(decision.crawler.is_some());

        // A block is never lifted for a crawler.
        let mut listed = page(LISTED_BLOCK);
        listed.user_agent = robots.user_agent;
        let mut decision = decide(&engine, ShieldMode::Attack, &listed);
        assert_eq!(decision.crawler, None);
        ShieldEngine::admit_verified_crawler(&mut decision);
        assert_eq!(decision.outcome, Outcome::Block);
    }

    #[test]
    fn identity_without_a_fingerprint_names_the_path_in() {
        let mut direct = page(CLEAN);
        direct.ja4 = None;
        let mut tunnel = direct;
        tunnel.source = "cloudflare";
        assert_ne!(direct.identity(), tunnel.identity());
        assert_ne!(direct.identity(), page(CLEAN).identity());
    }
}
