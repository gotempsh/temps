// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment and environment URLs, environment links and custom domains.

use super::*;

/// Everything needed to build the public Compose service URLs of a project's
/// environments. Empty `public_ports` means the project exposes no Compose
/// service publicly.
#[derive(Debug, Clone)]
pub struct ComposePublicUrlContext {
    pub app_settings: temps_core::AppSettings,
    pub public_ports: Vec<temps_entities::preset::ComposePublicPort>,
    /// Environment id -> `environments.subdomain`, the hostname label source.
    pub environment_subdomains: HashMap<i32, String>,
}

pub(super) fn deployment_url_from_settings(
    settings: &temps_core::AppSettings,
    proxy_port: u16,
    deployment_slug: &str,
) -> String {
    let domain = PublicHostnameStrategy::Standard
        .deployment_hostname(&settings.preview_domain, deployment_slug);
    let (protocol, port) = if let Some(external_url) = settings.external_url.as_deref() {
        if let Ok(parsed_url) = url::Url::parse(external_url) {
            let protocol = match parsed_url.scheme() {
                "https" => "https",
                _ => "http",
            };
            (protocol, parsed_url.port())
        } else {
            let protocol = if external_url.starts_with("https://") {
                "https"
            } else {
                "http"
            };
            (protocol, None)
        }
    } else {
        ("http", Some(proxy_port))
    };

    match port {
        Some(port)
            if !((protocol == "https" && port == 443) || (protocol == "http" && port == 80)) =>
        {
            format!("{protocol}://{domain}:{port}")
        }
        _ => format!("{protocol}://{domain}"),
    }
}

/// The links for an environment, best first; callers treat `[0]` as "the"
/// link (console Visit, last-deployment card, CLI deploy watcher).
///
/// 1. Active custom domains: DNS and certificate are verified.
/// 2. Hostnames the operator bound to the environment (`environment_domains`
///    rows other than the auto-managed one, whose value is the subdomain).
/// 3. The generated preview URL, last. On a default install it points at a
///    host such as `*.localho.st`, which resolves to the visitor's own machine,
///    so it must never win over a hostname the operator chose.
///
/// Hostnames are served by the same proxy listener as the generated URL, so
/// they take its scheme and port.
fn environment_links(
    generated_url: &str,
    subdomain: &str,
    custom_domains: &[String],
    bound_domains: &[String],
) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    let chosen = custom_domains.iter().chain(
        bound_domains
            .iter()
            .filter(|d| !d.eq_ignore_ascii_case(subdomain)),
    );
    for host in chosen {
        let link = on_listener_of(generated_url, host);
        if !links.iter().any(|l| l.eq_ignore_ascii_case(&link)) {
            links.push(link);
        }
    }
    if !links.iter().any(|l| l.eq_ignore_ascii_case(generated_url)) {
        links.push(generated_url.to_string());
    }
    links
}

/// An environment's links with its primary address first (ADR 068 D4).
#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedLinks {
    domains: Vec<String>,
    primary_url: Option<String>,
    no_address_reason: Option<String>,
}

/// Put the primary address first. A custom domain or private address is
/// served by the same listener as the generated URL, so it takes its scheme
/// and port; a tunnel host is served by the tunnel, on `https`. Without an
/// answer from reachability the stored order stays, as before ADR 068.
fn primary_first(
    links: Vec<String>,
    generated_url: &str,
    address: Option<&temps_core::environment_address::EnvironmentAddress>,
) -> OrderedLinks {
    use temps_core::environment_address::{https_url, AddressKind};
    let Some(address) = address else {
        return OrderedLinks {
            domains: links,
            primary_url: None,
            no_address_reason: None,
        };
    };
    let Some(primary) = &address.primary else {
        return OrderedLinks {
            domains: links,
            primary_url: None,
            no_address_reason: address.no_address_reason.clone(),
        };
    };
    let host_of = |link: &str| {
        url::Url::parse(link)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    };
    let wanted = primary.address.to_ascii_lowercase();
    let is_primary = |link: &String| host_of(link).as_deref() == Some(wanted.as_str());
    let url = match primary.kind {
        // A bound tunnel host is listed on the generated listener's scheme
        // and port, which is not how the tunnel serves it.
        AddressKind::TunnelHost => https_url(&primary.address),
        AddressKind::CustomDomain | AddressKind::Generated | AddressKind::Private => links
            .iter()
            .find(|link| is_primary(link))
            .cloned()
            .unwrap_or_else(|| on_listener_of(generated_url, &primary.address)),
    };
    let mut domains = vec![url.clone()];
    domains.extend(links.into_iter().filter(|l| !is_primary(l)));
    OrderedLinks {
        domains,
        primary_url: Some(url),
        no_address_reason: None,
    }
}

/// `host` as an absolute URL on the same scheme and port as `reference`.
/// A value that already carries a scheme is kept as is.
fn on_listener_of(reference: &str, host: &str) -> String {
    if host.contains("://") {
        return host.to_string();
    }
    let Ok(mut url) = url::Url::parse(reference) else {
        return format!("https://{host}");
    };
    if url.set_host(Some(host)).is_err() {
        return format!("https://{host}");
    }
    url.set_path("");
    url.set_query(None);
    url.as_str().trim_end_matches('/').to_string()
}

impl DeploymentService {
    /// Load what the handlers need to present a project's public Compose
    /// service URLs for the given environments. Non-Compose projects (and
    /// Compose projects without public ports) return an empty context without
    /// loading settings or environments.
    pub async fn compose_public_url_context(
        &self,
        project_id: i32,
        environment_ids: &[i32],
    ) -> Result<ComposePublicUrlContext, DeploymentError> {
        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "Project {project_id} not found while resolving public Compose service URLs"
                ))
            })?;
        let public_ports = match project.preset_config {
            Some(temps_entities::preset::PresetConfig::DockerCompose(config)) => {
                config.public_ports
            }
            _ => Vec::new(),
        };
        if public_ports.is_empty() || environment_ids.is_empty() {
            return Ok(ComposePublicUrlContext {
                app_settings: temps_core::AppSettings::default(),
                public_ports: Vec::new(),
                environment_subdomains: HashMap::new(),
            });
        }
        let environment_subdomains = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::Id.is_in(environment_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|environment| (environment.id, environment.subdomain))
            .collect();
        let app_settings = self.config_service.get_settings().await.map_err(|error| {
            DeploymentError::Other(format!(
                "Failed to load application settings for public Compose service URLs in project {project_id}: {error}"
            ))
        })?;
        Ok(ComposePublicUrlContext {
            app_settings,
            public_ports,
            environment_subdomains,
        })
    }

    pub async fn get_deployment_domains(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<DeploymentDomain>, DeploymentError> {
        let mut domains: Vec<DeploymentDomain> = Vec::new();

        // check if deployment_id is current in environments table
        let is_current = environments::Entity::find()
            .filter(environments::Column::CurrentDeploymentId.eq(Some(deployment_id)))
            .one(self.db.as_ref())
            .await?;

        if let Some(env) = is_current {
            domains.push(DeploymentDomain {
                id: 999999999,
                domain: env.subdomain,
            });
        }

        let db_domains = deployment_domains::Entity::find()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .all(self.db.as_ref())
            .await?;

        let db_domains_mapped: Vec<DeploymentDomain> = db_domains
            .into_iter()
            .map(|d| DeploymentDomain {
                id: d.id,
                domain: d.domain,
            })
            .collect();
        domains.extend(db_domains_mapped);
        Ok(domains)
    }

    /// `resolve`: judge each environment's primary address now (ADR 068
    /// D4), for a single deployment; otherwise only reuse a recent answer,
    /// so a list never resolves DNS per row.
    pub(super) async fn get_environments_with_domains(
        &self,
        environment_ids: &[i32],
        resolve: bool,
    ) -> Result<HashMap<i32, DeploymentEnvironment>, DeploymentError> {
        use temps_entities::{environment_domains, environments, project_custom_domains, projects};

        if environment_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let addresses = self.environment_addresses.get().and_then(|slot| slot.get());

        // Fetch all environments with their projects
        let environments = environments::Entity::find()
            .filter(environments::Column::Id.is_in(environment_ids.to_vec()))
            .find_also_related(projects::Entity)
            .all(self.db.as_ref())
            .await?;

        // Fetch all custom domains for these environments
        let custom_domains = project_custom_domains::Entity::find()
            .filter(project_custom_domains::Column::EnvironmentId.is_in(environment_ids.to_vec()))
            .filter(project_custom_domains::Column::Status.eq("active"))
            .all(self.db.as_ref())
            .await?;

        // Hostnames bound to the environments: what the proxy routes on
        let bound_domains = environment_domains::Entity::find()
            .filter(environment_domains::Column::EnvironmentId.is_in(environment_ids.to_vec()))
            .order_by_asc(environment_domains::Column::Id)
            .all(self.db.as_ref())
            .await?;

        // Group domains by environment_id
        let mut custom_by_env: HashMap<i32, Vec<String>> = HashMap::new();
        for domain in custom_domains {
            custom_by_env
                .entry(domain.environment_id)
                .or_default()
                .push(domain.domain);
        }
        let mut bound_by_env: HashMap<i32, Vec<String>> = HashMap::new();
        for domain in bound_domains {
            bound_by_env
                .entry(domain.environment_id)
                .or_default()
                .push(domain.domain);
        }

        // Build the result map
        let mut result = HashMap::new();
        for (env, _project) in environments {
            let custom = custom_by_env.remove(&env.id).unwrap_or_default();
            let bound = bound_by_env.remove(&env.id).unwrap_or_default();

            // Build the environment URL from the env's stored `subdomain`
            // (the canonical hostname source). Reconstructing from project_slug
            // and env_slug would produce stale URLs after a subdomain rename,
            // since `environments.subdomain` can be renamed independently.
            let env_url = self
                .compute_environment_url(&env.subdomain)
                .await
                .unwrap_or_else(|_| format!("http://{}.localhost", env.subdomain));
            let domains = environment_links(&env_url, &env.subdomain, &custom, &bound);
            let address = match &addresses {
                Some(addresses) if resolve => {
                    match addresses.address(env.project_id, env.id).await {
                        Ok(address) => Some(address),
                        Err(error) => {
                            warn!(
                                environment_id = env.id,
                                %error,
                                "could not tell the primary address; keeping the stored order"
                            );
                            None
                        }
                    }
                }
                Some(addresses) => addresses.cached(env.id),
                None => None,
            };
            let ordered = primary_first(domains, &env_url, address.as_ref());

            result.insert(
                env.id,
                DeploymentEnvironment {
                    id: env.id,
                    name: env.name,
                    slug: env.slug,
                    domains: ordered.domains,
                    primary_url: ordered.primary_url,
                    no_address_reason: ordered.no_address_reason,
                    reachability_warning: address.and_then(|a| a.warning),
                },
            );
        }

        Ok(result)
    }

    pub(super) async fn compute_deployment_url(
        &self,
        deployment_slug: &str,
    ) -> anyhow::Result<String> {
        let settings = self.config_service.get_settings().await.unwrap_or_default();
        Ok(deployment_url_from_settings(
            &settings,
            self.config_service.proxy_port(),
            deployment_slug,
        ))
    }

    pub async fn compute_environment_url(&self, env_subdomain: &str) -> anyhow::Result<String> {
        let settings = self.config_service.get_settings().await.unwrap_or_default();

        let domain = PublicHostnameStrategy::Standard
            .environment_hostname(&settings.preview_domain, env_subdomain);

        // Determine protocol and port from external_url if set, otherwise default to http
        let (protocol, port) = if let Some(ref url) = settings.external_url {
            if let Ok(parsed_url) = url::Url::parse(url) {
                let scheme = match parsed_url.scheme() {
                    "https" => "https",
                    "http" => "http",
                    _ => "http",
                };
                (scheme, parsed_url.port())
            } else {
                // Fallback for malformed URLs - detect protocol from prefix
                let protocol = if url.starts_with("https://") {
                    "https"
                } else {
                    "http"
                };
                (protocol, None)
            }
        } else {
            // No external_url: the public port IS the proxy listener port from
            // the Rust server config (e.g. :8080 on a local instance). Without
            // this the URL drops to :80 and is unreachable on a non-standard
            // port. `proxy_port()` is the single source of truth.
            ("http", Some(self.config_service.proxy_port()))
        };

        // Construct the URL with port if present
        // Only include port if it's non-standard (not 443 for https, not 80 for http)
        let url = if let Some(port) = port {
            let is_standard_port =
                (protocol == "https" && port == 443) || (protocol == "http" && port == 80);
            if is_standard_port {
                format!("{}://{}", protocol, domain)
            } else {
                format!("{}://{}:{}", protocol, domain, port)
            }
        } else {
            format!("{}://{}", protocol, domain)
        };

        Ok(url)
    }

    /// Add a custom domain to a deployment (marks it as not calculated)
    pub async fn add_custom_domain(
        &self,
        deployment_id: i32,
        domain: String,
    ) -> Result<deployment_domains::Model, DeploymentError> {
        // Check if deployment exists
        let _deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("Deployment {} not found", deployment_id))
            })?;

        // Remove any existing calculated domains for this deployment
        deployment_domains::Entity::delete_many()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_domains::Column::IsCalculated.eq(true))
            .exec(self.db.as_ref())
            .await?;

        // Add the custom domain
        let new_domain = deployment_domains::ActiveModel {
            deployment_id: Set(deployment_id),
            domain: Set(domain),
            is_calculated: Set(false), // This is a user-set custom domain
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let domain = new_domain.insert(self.db.as_ref()).await?;

        info!(
            "Added custom domain {} to deployment {}",
            domain.domain, deployment_id
        );
        Ok(domain)
    }

    /// Update deployment to use calculated wildcard domain
    pub async fn use_calculated_domain(
        &self,
        deployment_id: i32,
        project: &projects::Model,
        environment: &environments::Model,
    ) -> Result<deployment_domains::Model, DeploymentError> {
        // Get preview domain from config service
        let settings = self
            .config_service
            .get_settings()
            .await
            .map_err(|e| DeploymentError::Other(format!("Failed to get settings: {}", e)))?;

        // Get pipeline id from deployment
        let deployment = deployments::Entity::find_by_id(deployment_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("Deployment {} not found", deployment_id))
            })?;

        let deployment_label = deployment.id.to_string();
        let domain = PublicHostnameStrategy::Standard.project_deployment_hostname(
            &settings.preview_domain,
            &project.slug,
            &environment.slug,
            &deployment_label,
        );

        // Remove any existing domains for this deployment
        deployment_domains::Entity::delete_many()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .exec(self.db.as_ref())
            .await?;

        // Add the calculated domain
        let new_domain = deployment_domains::ActiveModel {
            deployment_id: Set(deployment_id),
            domain: Set(domain.clone()),
            is_calculated: Set(true), // This is a calculated wildcard domain
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let domain_model = new_domain.insert(self.db.as_ref()).await?;

        info!(
            "Updated deployment {} to use calculated domain {}",
            deployment_id, domain
        );
        Ok(domain_model)
    }

    /// Get all domains for a deployment with their type information
    pub async fn get_deployment_domains_with_type(
        &self,
        deployment_id: i32,
    ) -> Result<Vec<deployment_domains::Model>, DeploymentError> {
        let domains = deployment_domains::Entity::find()
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .all(self.db.as_ref())
            .await?;

        Ok(domains)
    }

    /// Remove a custom domain from a deployment
    pub async fn remove_custom_domain(
        &self,
        deployment_id: i32,
        domain_id: i32,
    ) -> Result<(), DeploymentError> {
        // Only allow removing non-calculated domains
        let domain = deployment_domains::Entity::find_by_id(domain_id)
            .filter(deployment_domains::Column::DeploymentId.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound("Domain not found".to_string()))?;

        if domain.is_calculated {
            return Err(DeploymentError::InvalidInput(
                "Cannot remove calculated domains. Use custom domain instead.".to_string(),
            ));
        }

        deployment_domains::Entity::delete_by_id(domain_id)
            .exec(self.db.as_ref())
            .await?;

        info!(
            "Removed custom domain {} from deployment {}",
            domain.domain, deployment_id
        );
        Ok(())
    }
}

#[cfg(test)]
mod environment_link_tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_chosen_hostname_wins_over_the_generated_url() {
        let links = environment_links(
            "http://shop-production.localho.st:8080",
            "shop-production",
            &s(&["shop.example.com"]),
            &s(&["shop-production", "www.shop.example.com"]),
        );
        assert_eq!(
            links,
            s(&[
                "http://shop.example.com:8080",
                "http://www.shop.example.com:8080",
                "http://shop-production.localho.st:8080",
            ])
        );
    }

    #[test]
    fn hostnames_take_the_listener_scheme_and_port() {
        let links = environment_links(
            "https://shop-production.apps.example.net",
            "shop-production",
            &[],
            &s(&["shop.example.com"]),
        );
        assert_eq!(links[0], "https://shop.example.com");
    }

    #[test]
    fn only_the_generated_url_when_nothing_is_bound() {
        // The auto-managed row carries the subdomain, not a hostname.
        let links = environment_links(
            "https://shop-production.apps.example.net",
            "shop-production",
            &[],
            &s(&["shop-production"]),
        );
        assert_eq!(links, s(&["https://shop-production.apps.example.net"]));
    }

    fn address(
        primary: Option<(&str, temps_core::environment_address::AddressKind)>,
        no_address: Option<&str>,
    ) -> temps_core::environment_address::EnvironmentAddress {
        temps_core::environment_address::EnvironmentAddress {
            environment_id: 1,
            primary: primary.map(
                |(host, kind)| temps_core::environment_address::PrimaryAddress {
                    address: host.to_string(),
                    kind,
                    way_in: None,
                },
            ),
            no_address_reason: no_address.map(str::to_string),
            warning: None,
        }
    }

    #[test]
    fn the_primary_address_leads_the_links() {
        use temps_core::environment_address::AddressKind;
        let generated = "http://shop-production.localho.st:8080";
        let links = environment_links(
            generated,
            "shop-production",
            &s(&["shop.example.com", "old.example.com"]),
            &[],
        );

        // No answer from reachability: the stored order, as before.
        let kept = primary_first(links.clone(), generated, None);
        assert_eq!(kept.domains, links);
        assert_eq!(kept.primary_url, None);

        // A reachable custom domain that is not first moves to the front.
        let ordered = primary_first(
            links.clone(),
            generated,
            Some(&address(
                Some(("old.example.com", AddressKind::CustomDomain)),
                None,
            )),
        );
        assert_eq!(
            ordered.domains,
            s(&[
                "http://old.example.com:8080",
                "http://shop.example.com:8080",
                generated,
            ])
        );
        assert_eq!(
            ordered.primary_url.as_deref(),
            Some("http://old.example.com:8080")
        );

        // A tunnel host is not among the stored links and is served on https.
        let ordered = primary_first(
            links.clone(),
            generated,
            Some(&address(
                Some(("shop-production.example.org", AddressKind::TunnelHost)),
                None,
            )),
        );
        assert_eq!(ordered.domains[0], "https://shop-production.example.org");
        assert_eq!(ordered.domains.len(), links.len() + 1);

        // A bound tunnel host is listed on the listener's http; it is still
        // advertised on https, and only once.
        let bound = environment_links(
            generated,
            "shop-production",
            &s(&["shop.example.com"]),
            &s(&["Shop-Production.example.org"]),
        );
        assert!(bound.contains(&"http://shop-production.example.org:8080".to_string()));
        let ordered = primary_first(
            bound.clone(),
            generated,
            Some(&address(
                Some(("shop-production.example.org", AddressKind::TunnelHost)),
                None,
            )),
        );
        assert_eq!(
            ordered.primary_url.as_deref(),
            Some("https://shop-production.example.org")
        );
        assert_eq!(ordered.domains[0], "https://shop-production.example.org");
        assert_eq!(ordered.domains.len(), bound.len());
        assert!(!ordered
            .domains
            .contains(&"http://shop-production.example.org:8080".to_string()));

        // Nobody can reach it: the links stay, the reason is reported.
        let none = primary_first(
            links.clone(),
            generated,
            Some(&address(None, Some("only resolves to this computer"))),
        );
        assert_eq!(none.domains, links);
        assert_eq!(none.primary_url, None);
        assert_eq!(
            none.no_address_reason.as_deref(),
            Some("only resolves to this computer")
        );
    }

    #[test]
    fn a_domain_listed_in_both_tables_appears_once() {
        let links = environment_links(
            "https://shop-production.apps.example.net",
            "shop-production",
            &s(&["shop.example.com"]),
            &s(&["SHOP.example.com"]),
        );
        assert_eq!(
            links,
            s(&[
                "https://shop.example.com",
                "https://shop-production.apps.example.net"
            ])
        );
    }
}
