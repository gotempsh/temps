// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /ingress/certificate-methods`: which ways to give one domain HTTPS
//! work on this cluster (ADR 058 D10, ADR 064 D4).
//!
//! A domain gets HTTPS through a tunnel that covers it, or from a Temps
//! certificate issued by HTTP-01 or DNS-01. That is decided per domain: a
//! tunnel on one domain never stops a certificate for another, and how the
//! console itself was reached (through Cloudflare, say) decides nothing.
//! Each method follows the capability pattern: `configured`, and when it is
//! not, the precise `reason` and the `setup_path` that fixes it.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use tracing::debug;
use utoipa::{IntoParams, OpenApi, ToSchema};

use super::service::{ReachabilityError, ReachabilityService};
use crate::error::IngressError;

/// Where a tunnel is set up (the console's Worker Nodes → ingress page).
pub const TUNNEL_SETUP_PATH: &str = "/settings/nodes/ingress?via=tunnel";
/// Where public ports 80 and 443 are opened to a node.
pub const PORTS_SETUP_PATH: &str = "/settings/nodes/ingress?via=ports";
/// Where a DNS provider is connected.
pub const DNS_PROVIDER_SETUP_PATH: &str = "/dns-providers/add";

/// One way to give a domain HTTPS, and whether it works here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MethodCapability {
    /// The method can be used for this domain now.
    pub configured: bool,
    /// Why it cannot, when `configured` is `false`.
    pub reason: Option<String>,
    /// The console path that sets up what is missing, when one can.
    pub setup_path: Option<String>,
}

impl MethodCapability {
    fn ready() -> Self {
        Self {
            configured: true,
            reason: None,
            setup_path: None,
        }
    }

    fn missing(reason: String, setup_path: Option<&str>) -> Self {
        Self {
            configured: false,
            reason: Some(reason),
            setup_path: setup_path.map(str::to_string),
        }
    }
}

/// Which certificate methods, and which tunnels, fit a domain here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CertificateMethods {
    /// `http01.configured`, kept for consoles that read only this.
    pub http01_available: bool,
    /// A Let's Encrypt certificate validated over public port 80.
    pub http01: MethodCapability,
    /// A Let's Encrypt certificate validated by a DNS TXT record that Temps
    /// writes through a connected DNS provider.
    pub dns01_provider: MethodCapability,
    /// A Let's Encrypt certificate validated by a DNS TXT record the
    /// operator adds by hand. Needs no inbound port and no provider.
    pub dns01_manual: MethodCapability,
    /// Served through a tunnel that covers the domain, its provider serving
    /// HTTPS, so no certificate is issued here.
    pub tunnel: MethodCapability,
    /// The DNS provider that would write the DNS-01 record.
    pub dns_provider: Option<String>,
    /// The tunnels that cover the domain, most specific first. Without a
    /// domain, every tunnel.
    pub tunnels: Vec<String>,
}

/// A tunnel, by what decides whether it covers a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelCoverage {
    pub name: String,
    pub domain: String,
}

/// What the methods are judged from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CertificateMethodFacts {
    /// The domain asked about, normalized, `*.` kept for a wildcard.
    pub domain: Option<String>,
    /// Something here answers on public port 80 (ADR 064 D4).
    pub http01_reachable: bool,
    /// The active provider with a verified, auto-managed zone covering the
    /// domain: the one automatic DNS-01 issuance and renewal write through
    /// (`DnsProviderService::find_provider_for_domain`). Without a domain,
    /// any active provider.
    pub dns_provider: Option<String>,
    /// Why no provider could be picked for the domain, when the lookup found
    /// a problem rather than nothing (two zones claiming it, say).
    pub dns_provider_problem: Option<String>,
    /// The tunnels covering the domain, most specific first; without a
    /// domain, every tunnel.
    pub covering_tunnels: Vec<String>,
    /// The tunnels that do not cover it.
    pub other_tunnels: Vec<TunnelCoverage>,
}

/// The methods `facts` allow, each with its reason when it does not fit.
pub fn certificate_methods(facts: &CertificateMethodFacts) -> CertificateMethods {
    let domain = facts.domain.as_deref();
    let wildcard = domain.is_some_and(|d| d.starts_with("*."));

    let http01 = if wildcard {
        MethodCapability::missing(
            "Let's Encrypt issues a wildcard certificate only through DNS-01.".to_string(),
            None,
        )
    } else if !facts.http01_reachable {
        MethodCapability::missing(
            "HTTP-01 needs Let's Encrypt to reach this server on public port 80, and visitors \
             to reach it on 443. Temps sees nothing answering publicly on port 80: HTTP-01 \
             still works if port 80 reaches this server, for example through port forwarding; \
             otherwise issue the certificate with DNS-01, or open ports 80 and 443 to a node."
                .to_string(),
            Some(PORTS_SETUP_PATH),
        )
    } else {
        MethodCapability::ready()
    };

    let dns01_provider = match (&facts.dns_provider, domain) {
        (Some(_), _) => MethodCapability::ready(),
        (None, Some(domain)) => MethodCapability::missing(
            match &facts.dns_provider_problem {
                Some(problem) => format!(
                    "Temps cannot pick a DNS provider for {domain}: {problem}. Add the DNS-01 TXT \
                     record by hand, or fix the provider's zones."
                ),
                None => format!(
                    "No connected DNS provider auto-manages a verified zone covering {domain}: \
                     add the DNS-01 TXT record by hand, or connect the provider that hosts its \
                     DNS (with automatic management on) so Temps writes it and renews it."
                ),
            },
            Some(DNS_PROVIDER_SETUP_PATH),
        ),
        (None, None) => MethodCapability::missing(
            "No DNS provider is connected: add DNS-01 TXT records by hand, or connect a provider \
             so Temps writes them."
                .to_string(),
            Some(DNS_PROVIDER_SETUP_PATH),
        ),
    };

    let tunnel = if !facts.covering_tunnels.is_empty() {
        MethodCapability::ready()
    } else {
        match (domain, facts.other_tunnels.as_slice()) {
            (Some(domain), []) => MethodCapability::missing(
                format!(
                    "No tunnel is set up, so {domain} gets HTTPS from a Temps certificate. Set up \
                     a tunnel on its domain to serve it with no open port."
                ),
                Some(TUNNEL_SETUP_PATH),
            ),
            (Some(domain), others) => MethodCapability::missing(
                format!(
                    "No tunnel covers {domain}: {}. Serve it with a Temps certificate, or set up \
                     a tunnel on its domain.",
                    others
                        .iter()
                        .map(|t| format!("tunnel {} carries names under {} only", t.name, t.domain))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                Some(TUNNEL_SETUP_PATH),
            ),
            (None, _) => MethodCapability::missing(
                "No tunnel is set up.".to_string(),
                Some(TUNNEL_SETUP_PATH),
            ),
        }
    };

    CertificateMethods {
        http01_available: http01.configured,
        http01,
        dns01_provider,
        dns01_manual: MethodCapability::ready(),
        tunnel,
        dns_provider: facts.dns_provider.clone(),
        tunnels: facts.covering_tunnels.clone(),
    }
}

/// `domain` normalized, `*.` kept, or why it is not a domain.
pub fn parse_domain(domain: &str) -> Result<String, ReachabilityError> {
    let trimmed = domain.trim();
    let (wildcard, base) = match trimmed.strip_prefix("*.") {
        Some(base) => (true, base),
        None => (false, trimmed),
    };
    let base = crate::service::normalize_host(base).map_err(|_| {
        ReachabilityError::Validation(format!(
            "{domain} is not a domain name; use one like example.com or *.example.com."
        ))
    })?;
    Ok(if wildcard { format!("*.{base}") } else { base })
}

/// Whether a tunnel on `tunnel_domain` (in `zone`) covers `domain`: the
/// names it publishes, or, for a wildcard, every name its domain has.
pub fn tunnel_covers(tunnel_domain: &str, zone: Option<&str>, domain: &str) -> bool {
    let tunnel_domain = tunnel_domain.trim_end_matches('.').to_ascii_lowercase();
    match domain.strip_prefix("*.") {
        Some(base) => base == tunnel_domain || base.ends_with(&format!(".{tunnel_domain}")),
        None => crate::service::covers(&tunnel_domain, zone.unwrap_or_default(), domain),
    }
}

/// The provider DNS-01 would write through for `domain`, or why none can be
/// picked. Uses the lookup automatic issuance and renewal use
/// (`find_provider_for_domain`: a verified, auto-managed zone of an active
/// provider), so the method is advertised only when that path can write the
/// record. Without a domain, any active provider.
pub async fn dns_provider_fact(
    providers: &temps_dns::DnsProviderService,
    domain: Option<&str>,
) -> Result<(Option<String>, Option<String>), ReachabilityError> {
    let Some(domain) = domain else {
        let active = providers.list_active().await.map_err(IngressError::from)?;
        return Ok((active.into_iter().next().map(|p| p.name), None));
    };
    match providers.find_provider_for_domain(domain).await {
        Ok(found) => Ok((found.map(|(provider, _)| provider.name), None)),
        // A misconfiguration the operator can fix: a reason, not a 500.
        Err(problem @ temps_dns::DnsError::AmbiguousManagedDomain { .. }) => {
            Ok((None, Some(problem.to_string())))
        }
        Err(other) => Err(IngressError::from(other).into()),
    }
}

/// Where the facts come from; the reachability service in production.
#[async_trait]
pub trait CertificateMethodSource: Send + Sync {
    async fn certificate_method_facts(
        &self,
        domain: Option<&str>,
    ) -> Result<CertificateMethodFacts, ReachabilityError>;
}

#[async_trait]
impl CertificateMethodSource for ReachabilityService {
    async fn certificate_method_facts(
        &self,
        domain: Option<&str>,
    ) -> Result<CertificateMethodFacts, ReachabilityError> {
        let domain = domain
            .filter(|d| !d.trim().is_empty())
            .map(parse_domain)
            .transpose()?;
        let http01_reachable = self.http01_available().await?;
        let (dns_provider, dns_provider_problem) =
            dns_provider_fact(&self.deps.dns_providers, domain.as_deref()).await?;
        let tunnels = self.deps.connectors.tunnel_facts(&[]).await?.tunnels;
        let mut covering = Vec::new();
        let mut other_tunnels = Vec::new();
        for tunnel in tunnels
            .into_iter()
            // A private network carries no public name.
            .filter(|t| !self.deps.connectors.serves_privately(&t.provider))
        {
            let covers = domain
                .as_deref()
                .is_none_or(|d| tunnel_covers(&tunnel.domain, tunnel.zone.as_deref(), d));
            if covers {
                covering.push((tunnel.domain.len(), tunnel.id, tunnel.name));
            } else {
                other_tunnels.push(TunnelCoverage {
                    name: tunnel.name,
                    domain: tunnel.domain,
                });
            }
        }
        covering.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        Ok(CertificateMethodFacts {
            domain,
            http01_reachable,
            dns_provider,
            dns_provider_problem,
            covering_tunnels: covering.into_iter().map(|(_, _, name)| name).collect(),
            other_tunnels,
        })
    }
}

pub struct CertificateMethodsState {
    pub source: Arc<dyn CertificateMethodSource>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CertificateMethodsQuery {
    /// The domain to judge, such as `my-app.example.com` or `*.example.com`.
    /// Without one, whether each method is set up at all.
    pub domain: Option<String>,
}

#[derive(OpenApi)]
#[openapi(
    paths(get_certificate_methods),
    components(schemas(CertificateMethods, MethodCapability)),
    tags((name = "Reachability", description = "Making a project reachable (ADR 064)"))
)]
pub struct CertificateMethodsApiDoc;

pub fn certificate_method_routes() -> Router<Arc<CertificateMethodsState>> {
    Router::new().route("/ingress/certificate-methods", get(get_certificate_methods))
}

/// Which ways to give a domain HTTPS work here.
///
/// Through a tunnel that covers it, or with a Temps certificate by HTTP-01
/// (needs public port 80) or DNS-01 (through a connected DNS provider, or a
/// TXT record added by hand). Each says why it does not fit and where to set
/// it up.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/ingress/certificate-methods",
    operation_id = "get_certificate_methods",
    params(CertificateMethodsQuery),
    responses(
        (status = 200, description = "The methods that fit the domain", body = CertificateMethods),
        (status = 400, description = "The domain is not a domain name"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "The facts could not be read"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_certificate_methods(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CertificateMethodsState>>,
    Query(query): Query<CertificateMethodsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);
    let facts = state
        .source
        .certificate_method_facts(query.domain.as_deref())
        .await?;
    debug!(
        domain = facts.domain.as_deref().unwrap_or(""),
        http01 = facts.http01_reachable,
        tunnels = facts.covering_tunnels.len(),
        "certificate methods judged"
    );
    Ok(Json(certificate_methods(&facts)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use temps_auth::{AuthContext, Permission, Role};
    use tower::ServiceExt;

    fn facts(domain: &str) -> CertificateMethodFacts {
        CertificateMethodFacts {
            domain: Some(domain.to_string()),
            ..Default::default()
        }
    }

    fn tunnel(name: &str, domain: &str) -> TunnelCoverage {
        TunnelCoverage {
            name: name.into(),
            domain: domain.into(),
        }
    }

    // ----- the judgement ------------------------------------------------

    #[test]
    fn tunnel_only_a_covered_domain_goes_through_the_tunnel_and_can_still_take_dns01() {
        let methods = certificate_methods(&CertificateMethodFacts {
            covering_tunnels: vec!["edge".into()],
            ..facts("my-app.apps.example.org")
        });
        assert!(methods.tunnel.configured);
        assert_eq!(methods.tunnels, vec!["edge".to_string()]);
        // Not reachable on 80: HTTP-01 says why and where to open it.
        assert!(!methods.http01.configured);
        assert!(!methods.http01_available);
        assert_eq!(methods.http01.setup_path.as_deref(), Some(PORTS_SETUP_PATH));
        assert!(methods.dns01_manual.configured);
    }

    #[test]
    fn direct_only_http01_works_and_no_tunnel_is_offered_with_its_setup() {
        let methods = certificate_methods(&CertificateMethodFacts {
            http01_reachable: true,
            ..facts("example.com")
        });
        assert!(methods.http01.configured);
        assert!(methods.http01_available);
        assert_eq!(methods.http01.reason, None);
        assert!(!methods.tunnel.configured);
        assert_eq!(
            methods.tunnel.setup_path.as_deref(),
            Some(TUNNEL_SETUP_PATH)
        );
        assert!(methods
            .tunnel
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("No tunnel is set up")));
        assert!(methods.dns01_manual.configured);
    }

    #[test]
    fn mixed_a_tunnel_on_one_domain_never_blocks_a_certificate_for_another() {
        let methods = certificate_methods(&CertificateMethodFacts {
            http01_reachable: true,
            dns_provider: Some("registrar".into()),
            other_tunnels: vec![tunnel("edge", "apps.example.org")],
            ..facts("example.com")
        });
        assert!(methods.http01.configured);
        assert!(methods.dns01_provider.configured);
        assert_eq!(methods.dns_provider.as_deref(), Some("registrar"));
        assert!(methods.dns01_manual.configured);
        // The tunnel is named with the only domain it carries.
        assert!(!methods.tunnel.configured);
        let reason = methods.tunnel.reason.unwrap_or_default();
        assert!(reason.contains("No tunnel covers example.com"), "{reason}");
        assert!(
            reason.contains("tunnel edge carries names under apps.example.org only"),
            "{reason}"
        );
    }

    #[test]
    fn no_inbound_80_443_http01_is_unavailable_and_dns01_is_offered_both_ways() {
        let no_provider = certificate_methods(&facts("example.com"));
        assert!(!no_provider.http01.configured);
        let reason = no_provider.http01.reason.clone().unwrap_or_default();
        assert!(reason.contains("port 80"), "{reason}");
        assert!(reason.contains("443"), "{reason}");
        assert!(reason.contains("DNS-01"), "{reason}");
        // The probe only advises: a forwarded port 80 still serves HTTP-01.
        assert!(reason.contains("port forwarding"), "{reason}");
        // By hand always works; through a provider says what to connect.
        assert!(no_provider.dns01_manual.configured);
        assert!(!no_provider.dns01_provider.configured);
        assert_eq!(
            no_provider.dns01_provider.setup_path.as_deref(),
            Some(DNS_PROVIDER_SETUP_PATH)
        );
        assert!(no_provider
            .dns01_provider
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("example.com")));

        let with_provider = certificate_methods(&CertificateMethodFacts {
            dns_provider: Some("registrar".into()),
            ..facts("example.com")
        });
        assert!(!with_provider.http01.configured);
        assert!(with_provider.dns01_provider.configured);
    }

    #[test]
    fn a_zone_claimed_twice_says_why_no_provider_writes_the_record() {
        let methods = certificate_methods(&CertificateMethodFacts {
            dns_provider_problem: Some("two zones claim example.com".into()),
            ..facts("my-app.example.com")
        });
        assert!(!methods.dns01_provider.configured);
        let reason = methods.dns01_provider.reason.unwrap_or_default();
        assert!(
            reason.contains("cannot pick a DNS provider for my-app.example.com"),
            "{reason}"
        );
        assert!(reason.contains("two zones claim example.com"), "{reason}");
        assert!(methods.dns01_manual.configured);
    }

    #[test]
    fn a_wildcard_is_dns01_only_whatever_the_ports() {
        let methods = certificate_methods(&CertificateMethodFacts {
            http01_reachable: true,
            ..facts("*.example.com")
        });
        assert!(!methods.http01.configured);
        assert_eq!(methods.http01.setup_path, None);
        assert!(methods.dns01_manual.configured);
    }

    #[test]
    fn without_a_domain_each_method_says_whether_it_is_set_up_at_all() {
        let methods = certificate_methods(&CertificateMethodFacts::default());
        assert!(!methods.tunnel.configured);
        assert!(!methods.dns01_provider.configured);
        assert!(methods.dns01_manual.configured);
    }

    #[test]
    fn coverage_follows_the_tunnels_publishing_rules() {
        // Names under it, and a subdomain tunnel's own name.
        assert!(tunnel_covers(
            "apps.example.org",
            Some("example.org"),
            "my-app.apps.example.org"
        ));
        assert!(tunnel_covers(
            "apps.example.org",
            Some("example.org"),
            "apps.example.org"
        ));
        // Never the zone apex, never another domain.
        assert!(!tunnel_covers(
            "example.org",
            Some("example.org"),
            "example.org"
        ));
        assert!(!tunnel_covers(
            "apps.example.org",
            Some("example.org"),
            "example.com"
        ));
        assert!(!tunnel_covers(
            "apps.example.org",
            Some("example.org"),
            "notapps.example.org"
        ));
        // A wildcard on its domain is "publish every name".
        assert!(tunnel_covers(
            "example.org",
            Some("example.org"),
            "*.example.org"
        ));
        assert!(!tunnel_covers(
            "apps.example.org",
            Some("example.org"),
            "*.example.org"
        ));
    }

    #[test]
    fn domains_are_normalized_and_garbage_is_refused() {
        assert_eq!(
            parse_domain(" My-App.Example.COM. ").ok().as_deref(),
            Some("my-app.example.com")
        );
        assert_eq!(
            parse_domain("*.Example.com").ok().as_deref(),
            Some("*.example.com")
        );
        let refused = parse_domain("https://example.com/path");
        assert!(matches!(
            refused,
            Err(ReachabilityError::Validation(message)) if message.contains("https://example.com/path")
        ));
    }

    // ----- the DNS provider lookup ----------------------------------------

    fn providers(db: sea_orm::MockDatabase) -> temps_dns::DnsProviderService {
        temps_dns::DnsProviderService::new(
            Arc::new(db.into_connection()),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "certificate-methods-test",
            )),
        )
    }

    fn zone(id: i32, provider_id: i32, domain: &str) -> temps_entities::dns_managed_domains::Model {
        let now = chrono::Utc::now();
        temps_entities::dns_managed_domains::Model {
            id,
            provider_id,
            domain: domain.into(),
            zone_id: None,
            auto_manage: true,
            proxied_by_default: false,
            verified: true,
            verified_at: Some(now),
            verification_error: None,
            generated_hostname_mode: "standard".into(),
            sync_generated_records: false,
            zone_access_ok: Some(true),
            zone_access_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn provider(id: i32, name: &str) -> temps_entities::dns_providers::Model {
        let now = chrono::Utc::now();
        temps_entities::dns_providers::Model {
            id,
            name: name.into(),
            provider_type: "cloudflare".into(),
            credentials: "not-read-here".into(),
            is_active: true,
            description: None,
            last_used_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// The capability and automatic issuance must agree: the lookup asks
    /// for an auto-managed zone, as `issue_dns01_through_provider` does.
    #[tokio::test]
    async fn the_provider_is_the_one_automatic_issuance_writes_through() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(vec![vec![(
                    zone(11, 101, "example.com"),
                    Some(provider(101, "registrar")),
                )]])
                .into_connection(),
        );
        let service = temps_dns::DnsProviderService::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "certificate-methods-test",
            )),
        );
        let (found, problem) = dns_provider_fact(&service, Some("my-app.example.com"))
            .await
            .expect("lookup succeeds");
        assert_eq!(found.as_deref(), Some("registrar"));
        assert_eq!(problem, None);
        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("the test holds the only connection")
            .into_transaction_log();
        let sql = &log[0].statements()[0].sql;
        assert!(
            sql.contains(r#""dns_managed_domains"."auto_manage" = "#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""dns_managed_domains"."verified" = "#),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn a_zone_only_auto_managed_lookups_skip_is_not_advertised() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<(
                temps_entities::dns_managed_domains::Model,
                Option<temps_entities::dns_providers::Model>,
            )>::new()]);
        let service = providers(db);
        let (found, problem) = dns_provider_fact(&service, Some("my-app.example.com"))
            .await
            .expect("lookup succeeds");
        assert_eq!(found, None);
        assert_eq!(problem, None);
    }

    #[tokio::test]
    async fn two_zones_claiming_the_domain_are_a_reason_not_a_failure() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                (zone(11, 101, "example.com"), Some(provider(101, "first"))),
                (zone(12, 202, "Example.COM."), Some(provider(202, "second"))),
            ]]);
        let service = providers(db);
        let (found, problem) = dns_provider_fact(&service, Some("my-app.example.com"))
            .await
            .expect("an ambiguous zone is reported, not raised");
        assert_eq!(found, None);
        assert!(problem.is_some_and(|p| p.contains("example.com")));
    }

    #[tokio::test]
    async fn a_database_failure_is_raised() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection reset".into())]);
        let service = providers(db);
        assert!(dns_provider_fact(&service, Some("my-app.example.com"))
            .await
            .is_err());
    }

    // ----- the handler --------------------------------------------------

    /// Answers fixed facts, or fails like an unreadable database.
    struct Fixed(Option<CertificateMethodFacts>);

    #[async_trait]
    impl CertificateMethodSource for Fixed {
        async fn certificate_method_facts(
            &self,
            domain: Option<&str>,
        ) -> Result<CertificateMethodFacts, ReachabilityError> {
            let domain = domain.map(parse_domain).transpose()?;
            match &self.0 {
                Some(facts) => Ok(CertificateMethodFacts {
                    domain,
                    ..facts.clone()
                }),
                None => Err(ReachabilityError::Database(sea_orm::DbErr::Custom(
                    "connection reset".into(),
                ))),
            }
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 5,
            name: "Operator".into(),
            email: "operator@example.test".into(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    async fn get(
        source: Fixed,
        uri: &str,
        auth: Option<AuthContext>,
    ) -> (StatusCode, serde_json::Value) {
        let app = certificate_method_routes().with_state(Arc::new(CertificateMethodsState {
            source: Arc::new(source),
        }));
        let mut request = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn reachable() -> Fixed {
        Fixed(Some(CertificateMethodFacts {
            http01_reachable: false,
            other_tunnels: vec![tunnel("edge", "apps.example.org")],
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn anonymous_callers_are_refused() {
        let (status, _) = get(reachable(), "/ingress/certificate-methods", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_key_without_domains_read_is_refused() {
        let key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::ProjectsRead]),
            "projects-only".into(),
            8,
        );
        let (status, _) = get(reachable(), "/ingress/certificate-methods", Some(key)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_reader_gets_the_methods_for_the_domain_asked_about() {
        let key = AuthContext::new_api_key(
            user(),
            None,
            Some(vec![Permission::DomainsRead]),
            "domains-reader".into(),
            9,
        );
        let (status, body) = get(
            reachable(),
            "/ingress/certificate-methods?domain=example.com",
            Some(key),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["http01_available"], false);
        assert_eq!(body["http01"]["configured"], false);
        assert_eq!(body["http01"]["setup_path"], PORTS_SETUP_PATH);
        assert_eq!(body["dns01_manual"]["configured"], true);
        assert_eq!(body["dns01_provider"]["configured"], false);
        assert_eq!(body["tunnel"]["configured"], false);
        assert!(body["tunnel"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("apps.example.org")));
    }

    #[tokio::test]
    async fn a_malformed_domain_is_a_validation_problem() {
        let admin = AuthContext::new_session(user(), Role::Admin);
        let (status, body) = get(
            reachable(),
            "/ingress/certificate-methods?domain=not%20a%20domain",
            Some(admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("not a domain")));
    }

    #[tokio::test]
    async fn unreadable_facts_are_a_server_problem_with_the_cause() {
        let admin = AuthContext::new_session(user(), Role::Admin);
        let (status, body) = get(
            Fixed(None),
            "/ingress/certificate-methods?domain=example.com",
            Some(admin),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("connection reset")));
    }
}
