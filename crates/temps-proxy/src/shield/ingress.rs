// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield on the control-plane proxy (ADR 066, I4): everything the request
//! path needs to call [`ShieldEngine::decide`], in one place.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use temps_entities::{environments, projects};
use temps_shield::crawler_check::CrawlerVerifier;
use temps_shield::events::bounded;
use temps_shield::{
    ChallengeGate, Decision, EnvPolicy, EventKind, EventSink, ExemptPaths, Index, NoEvents,
    Outcome, Request, ShieldEngine, ShieldEvent, ShieldMode,
};
use tracing::{debug, warn};

use super::{BypassTokens, ShieldLists};

/// Longest a request waits for a claimed crawler's DNS check (D7). A check
/// that takes longer counts as unverified: the request is challenged like
/// any other visitor's.
pub const CRAWLER_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

pub struct ProxyShield {
    pub(crate) engine: Arc<ShieldEngine>,
    pub(crate) lists: Arc<ShieldLists>,
    /// What this node adds to the lists on its own (netwatch's bans, phase
    /// 8). Empty until that reader is wired in.
    pub(crate) local: Arc<ArcSwapOption<Index>>,
    pub(crate) bypass: Arc<BypassTokens>,
    pub(crate) events: Arc<dyn EventSink>,
    /// Built on first use, on the runtime that serves requests; `None` when
    /// the system DNS configuration cannot be read.
    crawlers: OnceLock<Option<Arc<CrawlerVerifier>>>,
}

impl ProxyShield {
    pub fn new(gate: Arc<ChallengeGate>) -> Self {
        Self {
            engine: Arc::new(ShieldEngine::new(gate)),
            lists: Arc::new(ShieldLists::default()),
            local: Arc::new(ArcSwapOption::empty()),
            bypass: Arc::new(BypassTokens::default()),
            events: Arc::new(NoEvents),
            crawlers: OnceLock::new(),
        }
    }

    /// The mode the environment runs in: its own, else its project's, and
    /// `attack` while either legacy attack-mode column says so.
    pub fn mode(project: &projects::Model, environment: &environments::Model) -> ShieldMode {
        ShieldMode::effective(
            environment.shield_mode.as_deref(),
            &project.shield_mode,
            environment.attack_mode,
            project.attack_mode,
        )
    }

    pub(crate) fn set_crawler_verifier(&mut self, verifier: Arc<CrawlerVerifier>) {
        self.crawlers = OnceLock::from(Some(verifier));
    }

    /// The environment's Shield settings: its mode (inherited from the
    /// project, with the legacy attack-mode columns), its exempt paths and
    /// its bypass tokens.
    pub fn policy(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
    ) -> EnvPolicy {
        let mode = Self::mode(project, environment);
        // Off reads nothing else, so it costs nothing to build.
        if mode == ShieldMode::Off {
            return EnvPolicy {
                project_id: project.id,
                environment_id: environment.id,
                mode,
                ..EnvPolicy::default()
            };
        }
        EnvPolicy {
            project_id: project.id,
            environment_id: environment.id,
            mode,
            exempt_paths: ExemptPaths::lenient(&environment.shield_exempt_paths.0),
            bypass: self.bypass.tokens_for(project.id, environment.id),
        }
    }

    /// The decision for `request`, including the DNS check of a claimed
    /// crawler that would be challenged.
    pub async fn decide(&self, policy: &EnvPolicy, request: &Request<'_>, now: u64) -> Decision {
        let mut decision = {
            let index = self.lists.index();
            let local = self.local.load();
            self.engine
                .decide(&index, local.as_deref(), policy, request, now)
        };
        if let (
            Outcome::Challenge {
                crawler: Some(crawler),
                ..
            },
            Some(ip),
        ) = (&decision.outcome, request.ip)
        {
            if self.verify_crawler(ip, crawler).await {
                ShieldEngine::admit_verified_crawler(&mut decision);
            }
        }
        decision
    }

    async fn verify_crawler(
        &self,
        ip: IpAddr,
        crawler: &'static temps_shield::crawlers::Crawler,
    ) -> bool {
        let Some(verifier) = self
            .crawlers
            .get_or_init(|| match CrawlerVerifier::system() {
                Ok(verifier) => Some(Arc::new(verifier)),
                Err(error) => {
                    warn!(%error, "Claimed crawlers cannot be verified: no DNS resolver");
                    None
                }
            })
        else {
            return false;
        };
        match tokio::time::timeout(CRAWLER_CHECK_TIMEOUT, verifier.verify(ip, crawler)).await {
            Ok(verified) => verified,
            Err(_) => {
                debug!(%ip, crawler = crawler.name, "Crawler DNS check timed out; not verified");
                false
            }
        }
    }

    pub fn submit(&self, event: ShieldEvent) {
        self.events.submit(vec![event]);
    }
}

/// What the request is about, for an event.
pub struct EventContext<'a> {
    pub project_id: i32,
    pub environment_id: i32,
    pub request_id: &'a str,
    pub host: &'a str,
}

/// The event for `decision` (D9, D11), when [`Decision::is_event`].
pub fn decision_event(
    decision: &Decision,
    request: &Request<'_>,
    context: &EventContext<'_>,
) -> ShieldEvent {
    let kind = match decision.outcome {
        Outcome::Challenge { .. } => EventKind::ChallengeIssued,
        _ => EventKind::Verdict,
    };
    let mut event = base_event(kind, request, context);
    event.verdict = decision.verdict.action.as_str().to_string();
    event.mode = decision.mode.as_str().to_string();
    event.reasons = decision.verdict.reason_strings();
    event.signature = decision.verdict.signature().map(str::to_string);
    event.difficulty = decision.difficulty();
    event
}

/// The event for a verify request: solved, with how long the browser took,
/// or failed, with why. `mode` is the one the verify path's environment
/// runs in now.
pub fn verify_event(
    solved: Result<Option<u64>, &str>,
    mode: ShieldMode,
    request: &Request<'_>,
    context: &EventContext<'_>,
) -> ShieldEvent {
    let (kind, reasons, solve_ms) = match solved {
        Ok(solve_ms) => (
            EventKind::ChallengeSolved,
            Vec::new(),
            solve_ms.map(|ms| u32::try_from(ms).unwrap_or(u32::MAX)),
        ),
        Err(code) => (EventKind::ChallengeFailed, vec![code.to_string()], None),
    };
    let mut event = base_event(kind, request, context);
    event.verdict = "challenge".to_string();
    event.mode = mode.as_str().to_string();
    event.reasons = reasons;
    event.solve_ms = solve_ms;
    event
}

fn base_event(kind: EventKind, request: &Request<'_>, context: &EventContext<'_>) -> ShieldEvent {
    ShieldEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        project_id: context.project_id,
        environment_id: context.environment_id,
        // The control plane is no worker.
        node_id: None,
        request_id: context.request_id.to_string(),
        kind,
        ip: request.ip.map(|ip| ip.to_string()).unwrap_or_default(),
        ja4: request.ja4.map(str::to_string),
        verdict: String::new(),
        mode: String::new(),
        reasons: Vec::new(),
        signature: None,
        host: bounded(context.host),
        path: bounded(request.path),
        user_agent: bounded(request.user_agent),
        difficulty: None,
        solve_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use temps_shield::crawler_check::CrawlerDns;
    use temps_shield::{Admitted, ListVerdict, Network, ShieldSnapshot, SourceList};

    const GOOGLEBOT: &str =
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

    /// Answers for the addresses it knows; `delay` makes every answer late.
    #[derive(Default)]
    struct FakeDns {
        ptr: HashMap<IpAddr, Vec<String>>,
        a: HashMap<String, Vec<IpAddr>>,
        delay: Option<Duration>,
    }

    #[async_trait::async_trait]
    impl CrawlerDns for FakeDns {
        async fn reverse(&self, ip: IpAddr) -> Vec<String> {
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            self.ptr.get(&ip).cloned().unwrap_or_default()
        }
        async fn forward(&self, name: &str) -> Vec<IpAddr> {
            self.a.get(name).cloned().unwrap_or_default()
        }
    }

    fn shield(dns: FakeDns) -> ProxyShield {
        let now = temps_shield::unix_now() as i64;
        let mut shield = ProxyShield::new(Arc::new(crate::shield::test_gate()));
        shield.lists = Arc::new(ShieldLists::from_snapshot(&ShieldSnapshot {
            revision: 1,
            generated_at: now,
            lists: vec![SourceList::new(
                "dshield",
                ListVerdict::Challenge,
                now,
                None,
                Network::parse("66.249.64.0/19"),
            )],
            ..ShieldSnapshot::default()
        }));
        shield.set_crawler_verifier(Arc::new(CrawlerVerifier::new(Arc::new(dns))));
        shield
    }

    fn policy() -> EnvPolicy {
        EnvPolicy {
            project_id: 1,
            environment_id: 2,
            mode: ShieldMode::Selective,
            ..EnvPolicy::default()
        }
    }

    fn crawler_page(ip: IpAddr) -> Request<'static> {
        Request {
            ip: Some(ip),
            source: "direct",
            user_agent: GOOGLEBOT,
            method: "GET",
            path: "/",
            path_and_query: "/",
            accept: "text/html",
            tls: true,
            ..Request::default()
        }
    }

    #[tokio::test]
    async fn a_crawler_that_resolves_both_ways_passes_and_a_spoofed_one_is_challenged() {
        let real: IpAddr = "66.249.66.1".parse().unwrap();
        let spoofed: IpAddr = "66.249.66.2".parse().unwrap();
        let mut dns = FakeDns::default();
        dns.ptr
            .insert(real, vec!["crawl-66-249-66-1.googlebot.com.".into()]);
        dns.a
            .insert("crawl-66-249-66-1.googlebot.com.".into(), vec![real]);
        let shield = shield(dns);
        let now = temps_shield::unix_now();

        let decision = shield.decide(&policy(), &crawler_page(real), now).await;
        assert_eq!(decision.outcome, Outcome::Pass);
        assert_eq!(decision.admitted, Some(Admitted::VerifiedCrawler));
        assert!(decision.is_event(), "the verdict is still recorded");

        let decision = shield.decide(&policy(), &crawler_page(spoofed), now).await;
        assert!(matches!(decision.outcome, Outcome::Challenge { .. }));
    }

    /// A slow resolver cannot hold a request past the timeout; the crawler
    /// is then treated as unverified.
    #[tokio::test(start_paused = true)]
    async fn a_slow_dns_check_counts_as_unverified() {
        let real: IpAddr = "66.249.66.1".parse().unwrap();
        let mut dns = FakeDns {
            delay: Some(Duration::from_secs(30)),
            ..FakeDns::default()
        };
        dns.ptr
            .insert(real, vec!["crawl-66-249-66-1.googlebot.com.".into()]);
        dns.a
            .insert("crawl-66-249-66-1.googlebot.com.".into(), vec![real]);
        let shield = shield(dns);
        let started = tokio::time::Instant::now();
        let decision = shield
            .decide(&policy(), &crawler_page(real), temps_shield::unix_now())
            .await;
        assert!(matches!(decision.outcome, Outcome::Challenge { .. }));
        assert_eq!(started.elapsed(), CRAWLER_CHECK_TIMEOUT);
    }

    #[test]
    fn the_policy_follows_the_mode_columns_and_reads_exempt_paths() {
        let shield = ProxyShield::new(Arc::new(crate::shield::test_gate()));
        let project = projects::Model {
            id: 1,
            shield_mode: "selective".to_string(),
            ..test_project()
        };
        let mut environment = environments::Model {
            id: 2,
            project_id: 1,
            shield_mode: None,
            attack_mode: None,
            shield_exempt_paths: environments::ShieldExemptPaths(vec![
                "/api/*".to_string(),
                "not a path".to_string(),
            ]),
            ..test_environment()
        };
        let policy = shield.policy(&project, &environment);
        assert_eq!(policy.mode, ShieldMode::Selective);
        assert!(policy.exempt_paths.matches("/api/users"));
        environment.attack_mode = Some(true);
        assert_eq!(
            shield.policy(&project, &environment).mode,
            ShieldMode::Attack
        );
        environment.attack_mode = Some(false);
        environment.shield_mode = Some("off".to_string());
        let off = shield.policy(&project, &environment);
        assert_eq!(off.mode, ShieldMode::Off);
        assert!(off.exempt_paths.is_empty());
    }

    fn test_project() -> projects::Model {
        serde_json::from_value(serde_json::json!({
            "id": 1, "name": "app", "slug": "app", "repo_name": "app",
            "repo_owner": "example", "directory": ".", "main_branch": "main",
            "preset": "dockerfile", "created_at": chrono::Utc::now(),
            "updated_at": chrono::Utc::now(), "pull_only_root_directory": false,
            "is_deleted": false, "is_public_repo": false, "attack_mode": false,
            "shield_mode": "log",
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
        .expect("test project fixture deserializes")
    }

    fn test_environment() -> environments::Model {
        environments::Model {
            id: 2,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "prod".to_string(),
            last_deployment: None,
            host: String::new(),
            upstreams: temps_entities::upstream_config::UpstreamList::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            project_id: 1,
            current_deployment_id: None,
            branch: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            shield_mode: None,
            shield_exempt_paths: Default::default(),
            force_https: None,
            last_activity_at: None,
        }
    }
}
