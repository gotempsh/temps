// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Building the route table from the database: `load_routes` and the seven
//! phases it runs in order, each reading one source of routes into the
//! shared [`RouteLoadState`].

use super::*;
use sea_orm::{ColumnTrait, QueryFilter};
use temps_entities::{custom_routes, environment_domains, project_custom_domains, settings};

/// Everything one rebuild accumulates while its phases run, in the order
/// `load_routes` threads it through them.
struct RouteLoadState {
    /// Settings read once per rebuild.
    app_settings: AppSettings,
    preview_domain: String,
    /// Base domain -> public hostname strategy, from managed domains.
    hostname_strategies: HashMap<String, PublicHostnameStrategy>,
    /// Legacy exact routes (environment, custom-domain and discovered hosts).
    routes: HashMap<String, RouteInfo>,
    /// Hostnames whose backend is the environment's primary service.
    primary_route_hosts: HashSet<String>,
    sleeping_environments: Vec<SleepingEnvironmentEntry>,
    /// App hostnames with no live upstream, answered 503 by the proxy.
    unavailable: UnavailableHosts,
    /// Addresses whose audience is `private` (ADR 064 D5).
    private_meta: HashMap<String, PrivateAddress>,
    /// Addresses whose audience is `signed_in` (ADR 065 D7).
    gated_meta: HashMap<String, GatedMeta>,
    projects_cache: HashMap<i32, Arc<projects::Model>>,
    environments_cache: HashMap<i32, Arc<environments::Model>>,
    deployments_cache: HashMap<i32, Arc<deployments::Model>>,
    /// node_id -> private address, for multi-node routing.
    nodes_cache: HashMap<i32, String>,
    /// ADR 065 D6: who each `tailnet_users` address admits, read once.
    principals: Option<HashMap<String, Vec<temps_core::tailnet::TailnetPrincipal>>>,
    /// ADR 068 D3: the hosts a tunnel generated for each environment (phase
    /// 1), beside which phase 4 serves its public Compose services.
    tunnel_environment_hosts: HashMap<i32, Vec<String>>,
    /// Operator custom routes (phase 2), by route type.
    http_routes_map: HashMap<String, RouteInfo>,
    tls_routes_map: HashMap<String, RouteInfo>,
    http_wildcards_matcher: WildcardMatcher,
    tls_wildcards_matcher: WildcardMatcher,
    /// Status-page hostnames (phase 7).
    status_page_hosts: HashMap<String, StatusPageHostRoute>,
}

/// What `assemble_snapshot` hands back for publication.
struct AssembledRoutes {
    snapshot: Arc<RouteTableSnapshot>,
    sleeping_environments: Vec<SleepingEnvironmentEntry>,
    environments_cache: HashMap<i32, Arc<environments::Model>>,
}

impl CachedPeerTable {
    /// Load all routes from the database into the cache with full models.
    /// This queries environment_domains, custom_routes, and project_custom_domains.
    /// Returns a list of sleeping on-demand environments that were skipped during route loading.
    pub async fn load_routes(&self) -> Result<Vec<SleepingEnvironmentEntry>, RouteLoadError> {
        let _reload_guard = self.route_reload_lock.lock().await;

        let routes = HashMap::new();
        // Hostnames whose backend is the environment's PRIMARY service: the
        // route built from the first public Compose port (or the only
        // service of a non-Compose project). Per-service hostnames
        // (`<service>--<env>`) and custom domains pinned to a service are
        // never recorded. Path routes (ADR 050) resolve their target from
        // this set, so a secondary Compose service can never be selected by
        // hostname ordering.
        let primary_route_hosts: HashSet<String> = HashSet::new();
        let sleeping_environments: Vec<SleepingEnvironmentEntry> = Vec::new();
        // App hostnames skipped below because their deployment has no live
        // upstream. Served as a 503 by the proxy instead of the console.
        let unavailable = UnavailableHosts::default();
        // Addresses whose audience is `private` (ADR 064 D5): routed like any
        // other below, then moved out of everything this proxy serves.
        let private_meta: HashMap<String, PrivateAddress> = HashMap::new();
        // Addresses whose audience is `signed_in` (ADR 065 D7): served like
        // public ones, with the gate every listener puts in front of them.
        let gated_meta: HashMap<String, GatedMeta> = HashMap::new();

        // Build entity caches as we go - only cache what we actually need for routing
        let projects_cache: HashMap<i32, Arc<projects::Model>> = HashMap::new();
        let environments_cache: HashMap<i32, Arc<environments::Model>> = HashMap::new();
        let deployments_cache: HashMap<i32, Arc<deployments::Model>> = HashMap::new();
        // Node cache: maps node_id -> private_address for multi-node routing
        let nodes_cache: HashMap<i32, String> = HashMap::new();

        // Fetch preview-domain and hostname settings once per rebuild.
        let app_settings = settings::Entity::find()
            .one(self.db.as_ref())
            .await?
            .map(|s| AppSettings::from_json(s.data))
            .unwrap_or_default();
        let preview_domain = app_settings.preview_domain.clone();

        // Build a base-domain -> strategy map from managed domains once per
        // rebuild. Only the per-service hostname layout varies by strategy; env
        // and deployment hosts are strategy-independent.
        let hostname_strategies: std::collections::HashMap<String, PublicHostnameStrategy> =
            temps_entities::dns_managed_domains::Entity::find()
                .all(self.db.as_ref())
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|d| {
                    (
                        d.domain.to_ascii_lowercase(),
                        PublicHostnameStrategy::from_db_str(&d.generated_hostname_mode),
                    )
                })
                .collect();

        debug!(
            "Loaded public hostname settings: preview_domain={}, managed_domain_modes={}",
            preview_domain,
            hostname_strategies.len()
        );

        debug!("Loading route table from database...");

        // ADR 065 D6: who each `tailnet_users` address admits, read in one
        // query the first time such an address is found.
        let principals: Option<HashMap<String, Vec<temps_core::tailnet::TailnetPrincipal>>> = None;

        let mut state = RouteLoadState {
            app_settings,
            preview_domain,
            hostname_strategies,
            routes,
            primary_route_hosts,
            sleeping_environments,
            unavailable,
            private_meta,
            gated_meta,
            projects_cache,
            environments_cache,
            deployments_cache,
            nodes_cache,
            principals,
            tunnel_environment_hosts: HashMap::new(),
            http_routes_map: HashMap::new(),
            tls_routes_map: HashMap::new(),
            http_wildcards_matcher: WildcardMatcher::new(),
            tls_wildcards_matcher: WildcardMatcher::new(),
            status_page_hosts: HashMap::new(),
        };

        self.load_environment_domain_routes(&mut state).await?;
        self.load_custom_routes(&mut state).await?;
        self.load_project_custom_domain_routes(&mut state).await?;
        self.load_environment_routes(&mut state).await?;
        self.load_active_deployment_routes(&mut state).await?;
        self.load_discovered_routes(&mut state).await?;
        self.load_status_page_host_routes(&mut state).await?;

        let AssembledRoutes {
            snapshot: new_snapshot,
            sleeping_environments,
            environments_cache,
        } = self.assemble_snapshot(state).await?;

        // Atomically replace all route tables
        let route_count = new_snapshot.legacy.exact.len();
        let http_routes_count = new_snapshot.http_routes.len();
        let tls_routes_count = new_snapshot.tls_routes.len();
        let http_wildcards_count = new_snapshot.http_wildcards.len();
        let tls_wildcards_count = new_snapshot.tls_wildcards.len();
        let legacy_wildcards_count = new_snapshot.legacy.wildcards.len();
        let unavailable_count = new_snapshot.unavailable.len();

        // Collect on-demand configs for awake environments so the idle sweep can track them.
        let on_demand_configs: Vec<OnDemandConfigEntry> = environments_cache
            .values()
            .filter(|env| !env.sleeping)
            .filter_map(|env| {
                let dc = env.deployment_config.as_ref()?;
                if dc.on_demand {
                    Some(OnDemandConfigEntry {
                        environment_id: env.id,
                        idle_timeout_seconds: dc.idle_timeout_seconds,
                        wake_timeout_seconds: dc.wake_timeout_seconds,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Three-phase publication keeps route ownership conservative while
        // the separate sleeping-domain snapshot changes:
        // old routes/old sleeping -> old routes/(old ∪ new) ownership/new sleeping
        // -> new routes/new ownership/new sleeping. Thus a transition can only
        // suppress a wake briefly; it can never wake the wrong environment.
        let old_snapshot = self.route_snapshot.load_full();
        let mut transition = (*old_snapshot).clone();
        transition.ownership.include(&new_snapshot.ownership);
        self.route_snapshot.store(Arc::new(transition));

        let on_sleeping = self.on_sleeping_callback.lock().as_ref().cloned();
        if let Some(callback) = on_sleeping {
            callback(sleeping_environments.clone(), on_demand_configs);
        }

        self.route_snapshot.store(new_snapshot);

        if !sleeping_environments.is_empty() {
            info!(
                "Route table: {} sleeping on-demand environments skipped",
                sleeping_environments.len()
            );
        }

        info!(
            "Route table loaded with {} legacy routes; typed caches contain {} HTTP exact, {} TLS exact, {} HTTP wildcards, {} TLS wildcards, {} legacy wildcards; {} application hosts have no live backend",
            route_count, http_routes_count, tls_routes_count, http_wildcards_count, tls_wildcards_count, legacy_wildcards_count, unavailable_count
        );
        debug!(
            "Found {} on-demand configs for idle tracking",
            environments_cache
                .values()
                .filter(|environment| !environment.sleeping)
                .filter(|environment| environment
                    .deployment_config
                    .as_ref()
                    .is_some_and(|config| config.on_demand))
                .count()
        );

        // Bump the in-memory generation and wake any long-poll waiters
        // on the routes-sync endpoint, plus anything waiting for the first
        // load via `wait_until_loaded`. Order matters: bump first, then
        // notify, so a wake-up that races with a subsequent fetch always
        // sees the new value. Do this BEFORE the DNS reconcile so readiness
        // waiters are released as soon as the in-memory maps are live —
        // they must never be gated on DNS work.
        //
        // The authoritative process claims the number from the durable
        // counter, so it continues across restarts. Restarting at 1 would
        // leave every agent long-polling with a `since` above it -- not woken
        // by new generations until its 25s poll expires -- and leave their
        // old, higher ACKs satisfying the completion gate for routes they
        // never received. The claim is also the only write of that row, so
        // `mark_deployment_complete`'s target is always a generation the
        // snapshot endpoint has issued.
        self.loaded
            .store(true, std::sync::atomic::Ordering::Release);
        let published = self.advance_generation().await;
        debug!(
            generation = ?published.as_ref().ok(),
            role = ?self.route_generation_role(),
            "Route table reloaded"
        );
        // Always wake: first-load waiters key off `loaded`, and route-sync
        // long-polls re-check the generation and keep waiting if it has not
        // moved yet.
        self.generation_changed.notify_waiters();

        // Fire the async on-reload hook used by the deployment-DNS publisher
        // as a detached task. It's whole-set idempotent and must not block the
        // route load (or, by extension, the proxy's first-load readiness wait).
        // We snapshot the Arc out of the mutex first so the mutex isn't held
        // across the spawn.
        let on_reload = self.on_reload_callback.lock().as_ref().cloned();
        if let Some(callback) = on_reload {
            tokio::spawn(async move {
                callback().await;
            });
        }

        // Eager TLS pre-provisioning (ADR-018): fire the cert-eligible callback
        // with the stable per-environment hostnames from the just-loaded table.
        // The proxy wires the on-demand cert manager here so every new deployment
        // gets a cert issued immediately rather than on the first failing handshake.
        let on_cert_eligible = self.on_cert_eligible_callback.lock().as_ref().cloned();
        if let Some(callback) = on_cert_eligible {
            let cert_hosts: Vec<String> = self
                .route_snapshot
                .load()
                .legacy
                .exact
                .iter()
                .filter(|(host, r)| r.cert_eligible && !host.starts_with("*."))
                .map(|(host, _)| host.clone())
                .collect();
            if !cert_hosts.is_empty() {
                tokio::spawn(async move {
                    callback(cert_hosts).await;
                });
            }
        }

        // A reload whose generation was not claimed must not count as a
        // completed reload: callers publish `RouteTableUpdated` on success,
        // and the deployment completion gate would then compare workers'
        // old ACKs against the unchanged durable target and pass while they
        // still serve the previous routes. Failing here keeps the gate
        // waiting; it re-requests a reload, whose claim (or the background
        // retry) publishes the generation.
        match published {
            Ok(_) => Ok(sleeping_environments),
            Err(source) => Err(RouteLoadError::GenerationUnpublished {
                route_count: self.len(),
                source,
            }),
        }
    }

    /// Phase 1: `environment_domains` rows (e.g. `preview-123.temps.dev`).
    async fn load_environment_domain_routes(
        &self,
        state: &mut RouteLoadState,
    ) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            routes,
            primary_route_hosts,
            sleeping_environments,
            unavailable,
            private_meta,
            gated_meta,
            projects_cache,
            environments_cache,
            deployments_cache,
            nodes_cache,
            principals,
            tunnel_environment_hosts,
            ..
        } = state;

        // 1. Load environment_domains (e.g., preview-123.temps.dev)
        let env_domains = environment_domains::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 1: Loading {} environment domains",
            env_domains.len()
        );

        // ADR 068 D3: the hosts a tunnel generated for each environment. Its
        // public Compose services are served as `<label>--<service>` beside
        // each one (section 4).
        for env_domain in &env_domains {
            if env_domain.generated_by_connector_id.is_some()
                && env_domain.audience == temps_entities::audience::PUBLIC
            {
                tunnel_environment_hosts
                    .entry(env_domain.environment_id)
                    .or_default()
                    .push(env_domain.domain.to_ascii_lowercase());
            }
        }

        for env_domain in env_domains {
            if env_domain.audience == temps_entities::audience::SIGNED_IN {
                gated_meta.insert(
                    gate_host_key(&env_domain.domain),
                    GatedMeta::from_row(
                        temps_core::worker_ingress::private_address_id(false, env_domain.id),
                        env_domain.gate.as_deref(),
                        env_domain.gate_not_before,
                        env_domain.gate_access_provider_id,
                        env_domain.gate_access_aud.as_deref(),
                    ),
                );
            } else if env_domain.audience != temps_entities::audience::PUBLIC {
                let mut address = private_address(
                    &env_domain.audience,
                    temps_core::worker_ingress::private_address_id(false, env_domain.id),
                    env_domain.private_range.clone(),
                    env_domain.private_node_id,
                    env_domain.private_listener.clone(),
                    env_domain.private_via.as_deref(),
                );
                if let Some(list) = address.principals.as_mut() {
                    if principals.is_none() {
                        *principals = Some(load_tailnet_principals(self.db.as_ref()).await?);
                    }
                    *list = principals
                        .as_mut()
                        .and_then(|all| all.remove(&address.address_id))
                        .unwrap_or_default();
                }
                private_meta.insert(env_domain.domain.clone(), address);
            }
            // Fetch environment if not cached (skip soft-deleted)
            if !environments_cache.contains_key(&env_domain.environment_id) {
                if let Ok(Some(env)) = environments::Entity::find_by_id(env_domain.environment_id)
                    .filter(environments::Column::DeletedAt.is_null())
                    .one(self.db.as_ref())
                    .await
                {
                    environments_cache.insert(env.id, Arc::new(env));
                }
            }

            if let Some(environment) = environments_cache.get(&env_domain.environment_id) {
                if let Some(deployment_id) = environment.current_deployment_id {
                    // Skip sleeping on-demand environments — record them separately
                    if environment.sleeping {
                        let wake_timeout = environment
                            .deployment_config
                            .as_ref()
                            .map(|c| c.wake_timeout_seconds)
                            .unwrap_or(30);
                        sleeping_environments.push(SleepingEnvironmentEntry {
                            domain: env_domain.domain.clone(),
                            environment_id: environment.id,
                            project_id: environment.project_id,
                            deployment_id,
                            wake_timeout_seconds: wake_timeout,
                        });
                        debug!(
                            "Skipping sleeping environment domain: {} (env={}, deploy={})",
                            env_domain.domain, environment.id, deployment_id
                        );
                        continue;
                    }

                    // Fetch deployment if not cached
                    if !deployments_cache.contains_key(&deployment_id) {
                        if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            deployments_cache.insert(dep.id, Arc::new(dep));
                        }
                    }

                    if let Some(deployment) = deployments_cache.get(&deployment_id) {
                        // Load all active containers for this deployment
                        let containers = self.load_deployment_containers(deployment).await?;

                        // Fetch project if not cached
                        if !projects_cache.contains_key(&environment.project_id) {
                            if let Ok(Some(proj)) =
                                projects::Entity::find_by_id(environment.project_id)
                                    .one(self.db.as_ref())
                                    .await
                            {
                                projects_cache.insert(proj.id, Arc::new(proj));
                            }
                        }

                        let project = projects_cache.get(&environment.project_id);

                        // Determine backend type: static directory or upstream containers
                        let portless = !containers.is_empty()
                            && containers
                                .iter()
                                .all(|container| container.container_port.is_none());
                        if portless && deployment.static_dir_location.is_none() {
                            continue;
                        }
                        let backend = if let Some(static_dir) = &deployment.static_dir_location {
                            // Static deployment - serve from directory
                            Some(BackendType::StaticDir {
                                path: static_dir.clone(),
                            })
                        } else if deployment.state != "paused" && !containers.is_empty() {
                            let public_port = project
                                .and_then(|project| project.preset_config.as_ref())
                                .and_then(|config| match config {
                                    temps_entities::preset::PresetConfig::DockerCompose(config) => {
                                        config.public_ports.first()
                                    }
                                    _ => None,
                                });
                            match select_public_route_containers(&containers, public_port) {
                                // A Compose stack without a public port is private.
                                None if public_port.is_none() => continue,
                                // The public service has no running container.
                                None => None,
                                Some(route_containers) => {
                                    let mut backend_entries =
                                        Vec::with_capacity(route_containers.len());
                                    for c in route_containers {
                                        let node_addr = resolve_node_private_address(
                                            c.node_id,
                                            nodes_cache,
                                            self.db.as_ref(),
                                        )
                                        .await;
                                        let entry = match public_port {
                                            Some(port) => build_public_compose_backend_entry(
                                                c,
                                                node_addr.as_deref(),
                                                port,
                                                self.runtime_context.as_ref(),
                                            ),
                                            None => build_backend_entry(
                                                c,
                                                node_addr.as_deref(),
                                                self.runtime_context.as_ref(),
                                            ),
                                        };
                                        if let Some(entry) = entry {
                                            backend_entries.push(entry);
                                        }
                                    }
                                    (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                                        backends: backend_entries,
                                        round_robin_counter: Arc::new(AtomicUsize::new(0)),
                                    })
                                }
                            }
                        } else {
                            None
                        };

                        // No live backend (containers stopped, crashed or
                        // unreachable, or the deployment is paused): keep the
                        // host out of the routable maps, but remember it so
                        // the proxy answers 503 instead of the console.
                        let Some(backend) = backend else {
                            if !compose_without_public_ports(project.map(Arc::as_ref)) {
                                if let Some(route) =
                                    unavailable_route_for(project, environment, deployment)
                                {
                                    debug!(
                                        "Environment domain has no live backend: {} (project={}, env={}, deploy={}, reason={})",
                                        env_domain.domain, environment.project_id, environment.id, deployment_id, route.reason.as_str()
                                    );
                                    unavailable.record(&env_domain.domain, &route);
                                }
                            }
                            continue;
                        };

                        routes.insert(
                            env_domain.domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: project.cloned(),
                                environment: Some(Arc::clone(environment)),
                                deployment: Some(Arc::clone(deployment)),
                                // STABLE per-environment alias — certable (ADR-018 §2).
                                cert_eligible: true,
                            },
                        );
                        primary_route_hosts.insert(env_domain.domain.clone());

                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded environment domain route: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    env_domain.domain, addresses, addresses.len(), environment.project_id, environment.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded environment domain route (static): {} -> {} (project={}, env={}, deploy={})",
                                    env_domain.domain, path, environment.project_id, environment.id, deployment_id
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Phase 2: operator `custom_routes` (host:port mappings), split into HTTP and TLS routes.
    async fn load_custom_routes(&self, state: &mut RouteLoadState) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            routes,
            primary_route_hosts,
            http_routes_map,
            tls_routes_map,
            http_wildcards_matcher,
            tls_wildcards_matcher,
            ..
        } = state;

        // 2. Load custom_routes (custom domain mappings with host:port)
        // These are separated into HTTP and TLS routes based on route_type
        let custom_routes_data = custom_routes::Entity::find()
            .filter(custom_routes::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 2: Loading {} custom routes",
            custom_routes_data.len()
        );

        // Prepare route caches for custom_routes

        for custom_route in custom_routes_data {
            let backend_addr = format!("{}:{}", custom_route.host, custom_route.port);
            let route_info = RouteInfo {
                backend: BackendType::Upstream {
                    backends: vec![BackendEntry {
                        address: backend_addr.clone(),
                        container_id: None,
                        container_name: None,
                    }],
                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: None, // Custom routes don't have project context
                environment: None,
                deployment: None,
                // Operator-configured custom route mapping, not an on-demand zone
                // host — never trigger on-demand issuance (ADR-018 §2).
                cert_eligible: false,
            };

            let is_wildcard = custom_route.domain.starts_with("*.");
            let route_type_str = match custom_route.route_type {
                RouteType::Http => "http",
                RouteType::Tls => "tls",
            };

            match custom_route.route_type {
                RouteType::Http => {
                    if is_wildcard {
                        http_wildcards_matcher.insert(&custom_route.domain, route_info.clone());
                        debug!(
                            "Loaded HTTP wildcard custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    } else {
                        http_routes_map.insert(custom_route.domain.clone(), route_info.clone());
                        debug!(
                            "Loaded HTTP custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    }
                }
                RouteType::Tls => {
                    if is_wildcard {
                        tls_wildcards_matcher.insert(&custom_route.domain, route_info.clone());
                        debug!(
                            "Loaded TLS wildcard custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    } else {
                        tls_routes_map.insert(custom_route.domain.clone(), route_info.clone());
                        debug!(
                            "Loaded TLS custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    }
                }
            }

            // Also add to legacy routes map for backward compatibility
            primary_route_hosts.remove(&custom_route.domain);
            routes.insert(custom_route.domain.clone(), route_info);
        }

        Ok(())
    }

    /// Phase 3: `project_custom_domains` (custom domains with redirects or environment mapping).
    async fn load_project_custom_domain_routes(
        &self,
        state: &mut RouteLoadState,
    ) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            routes,
            primary_route_hosts,
            sleeping_environments,
            unavailable,
            private_meta,
            gated_meta,
            projects_cache,
            environments_cache,
            deployments_cache,
            nodes_cache,
            principals,
            ..
        } = state;

        // 3. Load project_custom_domains (custom domains with redirects or environment mapping)
        // Note: We load ALL custom domains regardless of status to allow immediate routing
        let custom_domains = project_custom_domains::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 3: Loading {} project custom domains",
            custom_domains.len()
        );

        for custom_domain in custom_domains {
            if custom_domain.audience == temps_entities::audience::SIGNED_IN {
                gated_meta.insert(
                    gate_host_key(&custom_domain.domain),
                    GatedMeta::from_row(
                        temps_core::worker_ingress::private_address_id(true, custom_domain.id),
                        custom_domain.gate.as_deref(),
                        custom_domain.gate_not_before,
                        custom_domain.gate_access_provider_id,
                        custom_domain.gate_access_aud.as_deref(),
                    ),
                );
            } else if custom_domain.audience != temps_entities::audience::PUBLIC {
                let mut address = private_address(
                    &custom_domain.audience,
                    temps_core::worker_ingress::private_address_id(true, custom_domain.id),
                    custom_domain.private_range.clone(),
                    custom_domain.private_node_id,
                    custom_domain.private_listener.clone(),
                    custom_domain.private_via.as_deref(),
                );
                if let Some(list) = address.principals.as_mut() {
                    if principals.is_none() {
                        *principals = Some(load_tailnet_principals(self.db.as_ref()).await?);
                    }
                    *list = principals
                        .as_mut()
                        .and_then(|all| all.remove(&address.address_id))
                        .unwrap_or_default();
                }
                private_meta.insert(custom_domain.domain.clone(), address);
            }
            // Fetch environment if not cached (skip soft-deleted)
            if !environments_cache.contains_key(&custom_domain.environment_id) {
                if let Ok(Some(env)) =
                    environments::Entity::find_by_id(custom_domain.environment_id)
                        .filter(environments::Column::DeletedAt.is_null())
                        .one(self.db.as_ref())
                        .await
                {
                    environments_cache.insert(env.id, Arc::new(env));
                }
            }

            if let Some(environment) = environments_cache.get(&custom_domain.environment_id) {
                if let Some(deployment_id) = environment.current_deployment_id {
                    // Skip sleeping on-demand environments — record them separately
                    if environment.sleeping {
                        let wake_timeout = environment
                            .deployment_config
                            .as_ref()
                            .map(|c| c.wake_timeout_seconds)
                            .unwrap_or(30);
                        sleeping_environments.push(SleepingEnvironmentEntry {
                            domain: custom_domain.domain.clone(),
                            environment_id: environment.id,
                            project_id: environment.project_id,
                            deployment_id,
                            wake_timeout_seconds: wake_timeout,
                        });
                        debug!(
                            "Skipping sleeping environment custom domain: {} (env={}, deploy={})",
                            custom_domain.domain, environment.id, deployment_id
                        );
                        continue;
                    }

                    // Fetch deployment if not cached
                    if !deployments_cache.contains_key(&deployment_id) {
                        if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            deployments_cache.insert(dep.id, Arc::new(dep));
                        }
                    }

                    if let Some(deployment) = deployments_cache.get(&deployment_id) {
                        // Load all active containers for this deployment
                        let containers = self.load_deployment_containers(deployment).await?;

                        // Fetch project if not cached
                        if !projects_cache.contains_key(&custom_domain.project_id) {
                            if let Ok(Some(proj)) =
                                projects::Entity::find_by_id(custom_domain.project_id)
                                    .one(self.db.as_ref())
                                    .await
                            {
                                projects_cache.insert(proj.id, Arc::new(proj));
                            }
                        }

                        let project = projects_cache.get(&custom_domain.project_id);

                        // Filter containers by service_name if specified (docker-compose service targeting)
                        let target_containers: Vec<_> =
                            if let Some(ref sn) = custom_domain.service_name {
                                containers
                                    .iter()
                                    .filter(|c| c.service_name.as_deref() == Some(sn.as_str()))
                                    .collect()
                            } else {
                                containers.iter().collect()
                            };

                        // Determine backend type: static directory or upstream containers
                        let portless = !containers.is_empty()
                            && containers
                                .iter()
                                .all(|container| container.container_port.is_none());
                        if portless && deployment.static_dir_location.is_none() {
                            continue;
                        }
                        let backend = if let Some(static_dir) = &deployment.static_dir_location {
                            // Static deployment - serve from directory
                            Some(BackendType::StaticDir {
                                path: static_dir.clone(),
                            })
                        } else if deployment.state != "paused" && !target_containers.is_empty() {
                            // Container deployment - proxy to containers
                            let mut backend_entries = Vec::with_capacity(target_containers.len());
                            for c in &target_containers {
                                let node_addr = resolve_node_private_address(
                                    c.node_id,
                                    nodes_cache,
                                    self.db.as_ref(),
                                )
                                .await;
                                if let Some(entry) = build_backend_entry(
                                    c,
                                    node_addr.as_deref(),
                                    self.runtime_context.as_ref(),
                                ) {
                                    backend_entries.push(entry);
                                }
                            }
                            (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                                backends: backend_entries,
                                round_robin_counter: Arc::new(AtomicUsize::new(0)),
                            })
                        } else {
                            None
                        };

                        // An operator-attached domain with no live backend is
                        // an outage, never a console URL (see section 1).
                        let Some(backend) = backend else {
                            if let Some(route) =
                                unavailable_route_for(project, environment, deployment)
                            {
                                debug!(
                                    "Custom domain has no live backend: {} (project={}, env={}, deploy={}, reason={})",
                                    custom_domain.domain, custom_domain.project_id, environment.id, deployment_id, route.reason.as_str()
                                );
                                unavailable.record(&custom_domain.domain, &route);
                            }
                            continue;
                        };

                        // A custom domain may be pinned to any service, so
                        // it is never the environment's primary route.
                        primary_route_hosts.remove(&custom_domain.domain);
                        routes.insert(
                            custom_domain.domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: custom_domain.redirect_to.clone(),
                                status_code: custom_domain.status_code,
                                project: project.cloned(),
                                environment: Some(Arc::clone(environment)),
                                deployment: Some(Arc::clone(deployment)),
                                // Operator-configured custom domain — out of the
                                // on-demand sslip.io zone; not on-demand certable
                                // (ADR-018 §2). Custom-domain TLS is provisioned
                                // through the normal manual/DNS-01 path.
                                cert_eligible: false,
                            },
                        );

                        if let Some(ref redirect) = custom_domain.redirect_to {
                            debug!(
                                "Loaded custom domain with redirect: {} -> {} (status: {:?})",
                                custom_domain.domain, redirect, custom_domain.status_code
                            );
                        } else {
                            match &backend {
                                BackendType::Upstream { backends, .. } => {
                                    let addresses: Vec<&str> =
                                        backends.iter().map(|b| b.address.as_str()).collect();
                                    debug!(
                                        "Loaded custom domain route: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                        custom_domain.domain, addresses, addresses.len(), custom_domain.project_id, environment.id, deployment_id
                                    );
                                }
                                BackendType::StaticDir { path } => {
                                    debug!(
                                        "Loaded custom domain route (static): {} -> {} (project={}, env={}, deploy={})",
                                        custom_domain.domain, path, custom_domain.project_id, environment.id, deployment_id
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Phase 4: every environment with a subdomain and a current deployment (preview domain routing).
    async fn load_environment_routes(
        &self,
        state: &mut RouteLoadState,
    ) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            preview_domain,
            hostname_strategies,
            routes,
            primary_route_hosts,
            sleeping_environments,
            unavailable,
            projects_cache,
            environments_cache,
            deployments_cache,
            nodes_cache,
            tunnel_environment_hosts,
            ..
        } = state;

        // 4. Load all environments with main_url (for preview domain routing)
        // This handles environments that don't have explicit environment_domains entries
        // Only fetch environments that have main_url and current_deployment_id
        let all_envs = environments::Entity::find()
            .filter(environments::Column::Subdomain.is_not_null())
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 4: Loading {} environments with main_url",
            all_envs.len()
        );

        for env in all_envs {
            if let Some(deployment_id) = env.current_deployment_id {
                let main_url = &env.subdomain;

                // Skip sleeping on-demand environments — record them separately
                if env.sleeping {
                    let wake_timeout = env
                        .deployment_config
                        .as_ref()
                        .map(|c| c.wake_timeout_seconds)
                        .unwrap_or(30);
                    // Record both the raw main_url and the full preview domain
                    sleeping_environments.push(SleepingEnvironmentEntry {
                        domain: main_url.clone(),
                        environment_id: env.id,
                        project_id: env.project_id,
                        deployment_id,
                        wake_timeout_seconds: wake_timeout,
                    });
                    let full_domain = PublicHostnameStrategy::Standard
                        .environment_hostname(preview_domain, main_url);
                    sleeping_environments.push(SleepingEnvironmentEntry {
                        domain: full_domain,
                        environment_id: env.id,
                        project_id: env.project_id,
                        deployment_id,
                        wake_timeout_seconds: wake_timeout,
                    });
                    // Public Compose service URLs (`<service>--<env>`, and
                    // `<service>-<port>--<env>` for extra ports) must wake the
                    // environment too, not only its main hostname.
                    if !projects_cache.contains_key(&env.project_id) {
                        if let Ok(Some(proj)) = projects::Entity::find_by_id(env.project_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            projects_cache.insert(proj.id, Arc::new(proj));
                        }
                    }
                    if let Some(project) = projects_cache.get(&env.project_id) {
                        let strategy = match_strategy(hostname_strategies, preview_domain);
                        for domain in compose_public_service_hostnames(
                            project.preset_config.as_ref(),
                            preview_domain,
                            strategy,
                            main_url,
                        ) {
                            sleeping_environments.push(SleepingEnvironmentEntry {
                                domain,
                                environment_id: env.id,
                                project_id: env.project_id,
                                deployment_id,
                                wake_timeout_seconds: wake_timeout,
                            });
                        }
                    }
                    debug!(
                        "Skipping sleeping environment: {} (env={}, deploy={})",
                        main_url, env.id, deployment_id
                    );
                    continue;
                }

                // Cache environment if not already cached
                environments_cache
                    .entry(env.id)
                    .or_insert_with(|| Arc::new(env.clone()));

                // Fetch deployment if not cached.
                // Accept any state — if current_deployment_id points here, it should be routable.
                // The previous "completed" filter caused a race: mark_deployment_complete sets
                // current_deployment_id (fires PG NOTIFY) BEFORE setting state="completed",
                // so the route table reload would skip the deployment and never confirm.
                if !deployments_cache.contains_key(&deployment_id) {
                    if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        deployments_cache.insert(dep.id, Arc::new(dep));
                    }
                }

                if let Some(deployment) = deployments_cache.get(&deployment_id) {
                    // Load all active containers for this deployment
                    use temps_entities::deployment_containers;
                    let containers = self.load_deployment_containers(deployment).await?;

                    // Fetch project if not cached
                    if !projects_cache.contains_key(&env.project_id) {
                        if let Ok(Some(proj)) = projects::Entity::find_by_id(env.project_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            projects_cache.insert(proj.id, Arc::new(proj));
                        }
                    }

                    let project = projects_cache.get(&env.project_id);
                    let environment = environments_cache.get(&env.id);

                    // Determine backend type: static directory or upstream containers
                    let portless = !containers.is_empty()
                        && containers
                            .iter()
                            .all(|container| container.container_port.is_none());
                    if portless && deployment.static_dir_location.is_none() {
                        continue;
                    }
                    let backend = if let Some(static_dir) = &deployment.static_dir_location {
                        // Static deployment - serve from directory
                        Some(BackendType::StaticDir {
                            path: static_dir.clone(),
                        })
                    } else if deployment.state != "paused" && !containers.is_empty() {
                        // For Compose deployments, the main route uses only
                        // the first explicitly configured public port. A stack
                        // with no public ports remains private instead of
                        // exposing whichever container happened to be
                        // discovered first. When the public service has no
                        // running container (or a stale service reference),
                        // nothing is routed and the hosts are reported
                        // unavailable below.
                        let is_compose = containers.iter().any(|c| c.service_name.is_some());
                        let (route_containers, public_port): (
                            Vec<&deployment_containers::Model>,
                            Option<ComposePublicPort>,
                        ) = if is_compose {
                            // Check for public_ports config
                            let first_public = project
                                .and_then(|p| p.preset_config.as_ref())
                                .and_then(|pc| {
                                    if let temps_entities::preset::PresetConfig::DockerCompose(
                                        cfg,
                                    ) = pc
                                    {
                                        cfg.public_ports.first().cloned()
                                    } else {
                                        None
                                    }
                                });

                            match first_public {
                                Some(pp) => {
                                    let cs: Vec<_> = containers
                                        .iter()
                                        .filter(|c| c.service_name.as_deref() == Some(&pp.service))
                                        .collect();
                                    (cs, Some(pp))
                                }
                                None => continue,
                            }
                        } else {
                            (containers.iter().collect(), None)
                        };

                        let mut backend_entries = Vec::with_capacity(route_containers.len());
                        for c in &route_containers {
                            let node_addr = resolve_node_private_address(
                                c.node_id,
                                nodes_cache,
                                self.db.as_ref(),
                            )
                            .await;
                            let entry = match public_port.as_ref() {
                                Some(port) => build_public_compose_backend_entry(
                                    c,
                                    node_addr.as_deref(),
                                    port,
                                    self.runtime_context.as_ref(),
                                ),
                                None => build_backend_entry(
                                    c,
                                    node_addr.as_deref(),
                                    self.runtime_context.as_ref(),
                                ),
                            };
                            if let Some(entry) = entry {
                                backend_entries.push(entry);
                            }
                        }
                        (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                            backends: backend_entries,
                            round_robin_counter: Arc::new(AtomicUsize::new(0)),
                        })
                    } else {
                        None
                    };

                    // No live backend: report every hostname this section
                    // would have routed for the environment as unavailable
                    // (see section 1), including the public Compose service
                    // URLs and the internal `*.temps.local` name.
                    let Some(backend) = backend else {
                        let route = environment
                            .filter(|_| !compose_without_public_ports(project.map(Arc::as_ref)))
                            .and_then(|environment| {
                                unavailable_route_for(project, environment, deployment)
                            });
                        if let Some(route) = route {
                            let mut hosts = vec![
                                main_url.clone(),
                                PublicHostnameStrategy::Standard
                                    .environment_hostname(preview_domain, main_url),
                            ];
                            let env_slug = env.slug.trim();
                            let proj_slug = route.project.slug.trim();
                            if !env_slug.is_empty() && !proj_slug.is_empty() {
                                hosts.push(format!("{}.{}.temps.local", env_slug, proj_slug));
                            }
                            hosts.extend(compose_public_service_hostnames(
                                route.project.preset_config.as_ref(),
                                preview_domain,
                                match_strategy(hostname_strategies, preview_domain),
                                main_url,
                            ));
                            debug!(
                                "Environment has no live backend: {:?} (project={}, env={}, deploy={}, reason={})",
                                hosts, env.project_id, env.id, deployment_id, route.reason.as_str()
                            );
                            for host in &hosts {
                                unavailable.record(host, &route);
                            }
                        }
                        continue;
                    };

                    // Add route with main_url as-is
                    if !routes.contains_key(main_url) {
                        routes.insert(
                            main_url.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: project.cloned(),
                                environment: environment.cloned(),
                                deployment: Some(Arc::clone(deployment)),
                                // STABLE per-environment hostname — certable (ADR-018 §2).
                                cert_eligible: true,
                            },
                        );
                        primary_route_hosts.insert(main_url.clone());
                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded environment route: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    main_url, addresses, addresses.len(), env.project_id, env.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded environment route (static): {} -> {} (project={}, env={}, deploy={})",
                                    main_url, path, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }

                    // Also add route with preview_domain suffix if configured
                    let full_domain = PublicHostnameStrategy::Standard
                        .environment_hostname(preview_domain, main_url);
                    if !routes.contains_key(&full_domain) {
                        routes.insert(
                            full_domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: project.cloned(),
                                environment: environment.cloned(),
                                deployment: Some(Arc::clone(deployment)),
                                // STABLE per-environment preview hostname —
                                // certable (ADR-018 §2). This is the env alias,
                                // one per environment, not a per-deployment name.
                                cert_eligible: true,
                            },
                        );
                        primary_route_hosts.insert(full_domain.clone());
                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded environment route with preview domain: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    full_domain, addresses, addresses.len(), env.project_id, env.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded environment route with preview domain (static): {} -> {} (project={}, env={}, deploy={})",
                                    full_domain, path, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }

                    // ADR-012-lite: stable per-deployment FQDN under the
                    // internal `*.temps.local` zone. Format
                    // `<env-slug>.<project-slug>.temps.local` resolves to
                    // the edge proxy and fans out to whatever containers
                    // are currently `running`+`ready_at IS NOT NULL` for
                    // this deployment, so client redeploys are invisible
                    // even when the client caches DNS aggressively.
                    //
                    // Skip when either slug is missing or empty; without
                    // both we can't construct a non-ambiguous label, and
                    // emitting `..temps.local` would clobber the parent
                    // zone. Slug uniqueness is enforced at create time
                    // for environments and projects so collisions inside
                    // a project are impossible.
                    if let Some(proj) = project {
                        let env_slug = env.slug.trim();
                        let proj_slug = proj.slug.trim();
                        if !env_slug.is_empty() && !proj_slug.is_empty() {
                            let internal_fqdn = format!("{}.{}.temps.local", env_slug, proj_slug);
                            if !routes.contains_key(&internal_fqdn) {
                                routes.insert(
                                    internal_fqdn.clone(),
                                    RouteInfo {
                                        backend: backend.clone(),
                                        redirect_to: None,
                                        status_code: None,
                                        project: project.cloned(),
                                        environment: environment.cloned(),
                                        deployment: Some(Arc::clone(deployment)),
                                        // Internal-only `*.temps.local` name — not
                                        // publicly reachable by Let's Encrypt, so
                                        // never on-demand certable (ADR-018 §2).
                                        cert_eligible: false,
                                    },
                                );
                                primary_route_hosts.insert(internal_fqdn.clone());
                                debug!(
                                    "Loaded internal temps.local route: {} (project={}, env={}, deploy={})",
                                    internal_fqdn, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }

                    // Docker Compose: create per-service routes ONLY for explicitly public ports.
                    // All ports are private by default — users must mark ports as public
                    // in the project's preset_config.public_ports.
                    let has_compose_services = containers.iter().any(|c| c.service_name.is_some());
                    if has_compose_services {
                        // Read public_ports from project's preset_config
                        let public_ports: Vec<ComposePublicPort> = project
                            .and_then(|p| p.preset_config.as_ref())
                            .and_then(|pc| {
                                if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc
                                {
                                    Some(cfg.public_ports.clone())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default();

                        if !public_ports.is_empty() {
                            // Group containers by service_name
                            let mut services: HashMap<String, Vec<&deployment_containers::Model>> =
                                HashMap::new();
                            for c in &containers {
                                if let Some(ref svc) = c.service_name {
                                    services.entry(svc.clone()).or_default().push(c);
                                }
                            }

                            let route_labels =
                                temps_entities::preset::compose_public_route_labels(&public_ports);
                            // A public service with no running container (or
                            // no reachable port) is down while the rest of
                            // the stack runs: its URL answers 503.
                            let svc_unavailable = environment.and_then(|environment| {
                                unavailable_route_for(project, environment, deployment)
                            });
                            let svc_strategy = match_strategy(hostname_strategies, preview_domain);
                            for (public_port, route_label) in public_ports.iter().zip(&route_labels)
                            {
                                let svc_domain = svc_strategy.service_hostname(
                                    preview_domain,
                                    main_url,
                                    route_label,
                                );
                                let svc_containers = match services.get(&public_port.service) {
                                    Some(c) => c,
                                    None => {
                                        if let Some(route) = &svc_unavailable {
                                            unavailable.record(&svc_domain, route);
                                        }
                                        continue;
                                    }
                                };

                                let mut svc_backends = Vec::with_capacity(svc_containers.len());
                                for c in svc_containers {
                                    let node_addr = resolve_node_private_address(
                                        c.node_id,
                                        nodes_cache,
                                        self.db.as_ref(),
                                    )
                                    .await;
                                    let Some(entry) = build_public_compose_backend_entry(
                                        c,
                                        node_addr.as_deref(),
                                        public_port,
                                        self.runtime_context.as_ref(),
                                    ) else {
                                        warn!(
                                            service = %public_port.service,
                                            configured_target = public_port.port,
                                            recorded_target = ?c.container_port,
                                            recorded_host_port = ?c.host_port,
                                            "Skipping public Compose route without a matching live Docker port mapping"
                                        );
                                        continue;
                                    };
                                    svc_backends.push(entry);
                                }

                                if svc_backends.is_empty() {
                                    if let Some(route) = &svc_unavailable {
                                        unavailable.record(&svc_domain, route);
                                    }
                                    continue;
                                }

                                let svc_backend = BackendType::Upstream {
                                    backends: svc_backends.clone(),
                                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                                };

                                let svc_route_info = RouteInfo {
                                    backend: svc_backend,
                                    redirect_to: None,
                                    status_code: None,
                                    project: project.cloned(),
                                    environment: environment.cloned(),
                                    deployment: Some(Arc::clone(deployment)),
                                    // STABLE per-service env hostname
                                    // (`<service>--<env>.<preview>`, or
                                    // `<service>-<port>--<env>` for a service's
                                    // additional public ports) — one per
                                    // environment+route, certable (ADR-018 §2).
                                    cert_eligible: true,
                                };

                                // ADR 068 D3: the same service beside each
                                // tunnel host, in the Flat form the tunnel's
                                // zone takes. Never displaces a route.
                                for tunnel_svc_domain in tunnel_service_hostnames(
                                    tunnel_environment_hosts.get(&env.id),
                                    route_label,
                                ) {
                                    routes
                                        .entry(tunnel_svc_domain)
                                        .or_insert_with(|| svc_route_info.clone());
                                }

                                if let std::collections::hash_map::Entry::Vacant(e) =
                                    routes.entry(svc_domain.clone())
                                {
                                    let addresses: Vec<&str> =
                                        svc_backends.iter().map(|b| b.address.as_str()).collect();
                                    debug!(
                                        "Loaded compose public port route: {} -> {:?} (service={}, target_port={}, published_port={:?}, project={}, env={})",
                                        svc_domain,
                                        addresses,
                                        public_port.service,
                                        public_port.port,
                                        public_port.published,
                                        env.project_id,
                                        env.id
                                    );
                                    e.insert(svc_route_info);
                                }
                            }
                        }
                    }
                }
            }
        }

        debug!(
            "Loaded {} projects, {} environments, {} deployments into cache (on-demand)",
            projects_cache.len(),
            environments_cache.len(),
            deployments_cache.len()
        );

        Ok(())
    }

    /// Phase 5: the per-deployment fallback hostname of every active deployment.
    async fn load_active_deployment_routes(
        &self,
        state: &mut RouteLoadState,
    ) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            preview_domain,
            routes,
            primary_route_hosts,
            unavailable,
            projects_cache,
            environments_cache,
            deployments_cache,
            nodes_cache,
            ..
        } = state;

        // 5. Load all active deployments for all environments
        // This ensures we have complete coverage of all running deployments
        debug!("Loading all active deployments for environments...");

        // Get all environments with current_deployment_id (exclude soft-deleted)
        let all_active_envs = environments::Entity::find()
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for env in all_active_envs {
            // Skip sleeping on-demand environments — they are already recorded
            if env.sleeping {
                continue;
            }

            // Cache environment if not already cached
            environments_cache
                .entry(env.id)
                .or_insert_with(|| Arc::new(env.clone()));

            if let Some(deployment_id) = env.current_deployment_id {
                // Fetch deployment if not cached (accept any state — same rationale as section 4)
                if !deployments_cache.contains_key(&deployment_id) {
                    if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        deployments_cache.insert(dep.id, Arc::new(dep));
                    }
                }

                // Fetch project if not cached
                if !projects_cache.contains_key(&env.project_id) {
                    if let Ok(Some(proj)) = projects::Entity::find_by_id(env.project_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        projects_cache.insert(proj.id, Arc::new(proj));
                    }
                }

                // Check if we have all required data cached
                if let (Some(deployment), Some(project), Some(environment)) = (
                    deployments_cache.get(&deployment_id),
                    projects_cache.get(&env.project_id),
                    environments_cache.get(&env.id),
                ) {
                    // Load all active containers for this deployment
                    let containers = self.load_deployment_containers(deployment).await?;

                    // Determine backend type: static directory or upstream containers
                    let portless = !containers.is_empty()
                        && containers
                            .iter()
                            .all(|container| container.container_port.is_none());
                    if portless && deployment.static_dir_location.is_none() {
                        continue;
                    }
                    let backend = if let Some(static_dir) = &deployment.static_dir_location {
                        // Static deployment - serve from directory
                        Some(BackendType::StaticDir {
                            path: static_dir.clone(),
                        })
                    } else if deployment.state != "paused" && !containers.is_empty() {
                        let public_port =
                            project
                                .preset_config
                                .as_ref()
                                .and_then(|config| match config {
                                    temps_entities::preset::PresetConfig::DockerCompose(config) => {
                                        config.public_ports.first()
                                    }
                                    _ => None,
                                });
                        match select_public_route_containers(&containers, public_port) {
                            // A Compose stack without a public port is private.
                            None if public_port.is_none() => continue,
                            // The public service has no running container.
                            None => None,
                            Some(route_containers) => {
                                let mut backend_entries =
                                    Vec::with_capacity(route_containers.len());
                                for c in route_containers {
                                    let node_addr = resolve_node_private_address(
                                        c.node_id,
                                        nodes_cache,
                                        self.db.as_ref(),
                                    )
                                    .await;
                                    let entry = match public_port {
                                        Some(port) => build_public_compose_backend_entry(
                                            c,
                                            node_addr.as_deref(),
                                            port,
                                            self.runtime_context.as_ref(),
                                        ),
                                        None => build_backend_entry(
                                            c,
                                            node_addr.as_deref(),
                                            self.runtime_context.as_ref(),
                                        ),
                                    };
                                    if let Some(entry) = entry {
                                        backend_entries.push(entry);
                                    }
                                }
                                (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                                    backends: backend_entries,
                                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                                })
                            }
                        }
                    } else {
                        None
                    };

                    // Generate a fallback route using deployment slug if no other routes exist
                    // This ensures every active deployment is accessible
                    let fallback_domain = PublicHostnameStrategy::Standard
                        .deployment_hostname(preview_domain, &deployment.slug);

                    // No live backend: the deployment URL answers 503 (see
                    // section 1) rather than falling through to the console.
                    let Some(backend) = backend else {
                        if !compose_without_public_ports(Some(project.as_ref())) {
                            if let Some(route) =
                                unavailable_route_for(Some(project), environment, deployment)
                            {
                                debug!(
                                    "Deployment fallback host has no live backend: {} (project={}, env={}, deploy={}, reason={})",
                                    fallback_domain, env.project_id, env.id, deployment_id, route.reason.as_str()
                                );
                                unavailable.record(&fallback_domain, &route);
                            }
                        }
                        continue;
                    };

                    if !routes.contains_key(&fallback_domain) {
                        routes.insert(
                            fallback_domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: Some(Arc::clone(project)),
                                environment: Some(Arc::clone(environment)),
                                deployment: Some(Arc::clone(deployment)),
                                // EPHEMERAL per-deployment fallback hostname
                                // (`<deployment-slug>.<preview>`) — a new name on
                                // every deploy. NEVER on-demand certable: it would
                                // churn certs against the shared sslip.io bucket
                                // and trip LE per-hostname limits (ADR-018 §2).
                                cert_eligible: false,
                            },
                        );
                        primary_route_hosts.insert(fallback_domain.clone());
                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded fallback route for active deployment: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    fallback_domain, addresses, addresses.len(), env.project_id, env.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded fallback route for active deployment (static): {} -> {} (project={}, env={}, deploy={})",
                                    fallback_domain, path, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }
                }
            }
        }

        debug!("Loaded all active deployments. Final cache: {} projects, {} environments, {} deployments",
            projects_cache.len(), environments_cache.len(), deployments_cache.len());

        Ok(())
    }

    /// Phase 6: Traefik-label discovered routes, on hosts no earlier phase claimed.
    async fn load_discovered_routes(
        &self,
        state: &mut RouteLoadState,
    ) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            routes,
            http_routes_map,
            tls_routes_map,
            http_wildcards_matcher,
            tls_wildcards_matcher,
            ..
        } = state;

        // 6. Load Traefik-label discovered routes (containers Temps did NOT
        // deploy — an operator's existing docker-compose/Coolify/Dokploy
        // stack). Written by `temps_deployer::traefik_discovery`; opt-in via
        // TEMPS_TRAEFIK_DISCOVERY_ENABLED, so this section is empty on a
        // default install.
        //
        // Loaded LAST on purpose. The discovery reconciler already refuses to
        // write a host owned by a deployment, custom route, custom domain, or
        // environment subdomain, but that check races a concurrent write and
        // cannot see rows another control plane node added a millisecond ago.
        // This merge is the authoritative precedence rule: a discovered route
        // is only ever placed on a hostname that no DB-driven route claimed in
        // this same rebuild. An adversarial (or merely careless) workload can
        // therefore never take a real deployment's domain by relabelling
        // itself — the worst it can do is fail to be routed and get logged.
        //
        // Scoped to the network this node is *currently* configured to adopt
        // from, and skipped entirely when discovery is off here. Without that
        // scope, turning discovery off (or repointing it at another network)
        // would leave every previously-adopted row still routing forever: the
        // rows outlive the configuration that created them, and only the
        // reconciler — which no longer runs — would ever delete them.
        let discovery_network = self.traefik_discovery_network.read().clone();
        let discovered_routes = match discovery_network.as_deref() {
            Some(network) => {
                temps_entities::traefik_discovered_routes::Entity::find()
                    .filter(temps_entities::traefik_discovered_routes::Column::Enabled.eq(true))
                    .filter(
                        temps_entities::traefik_discovered_routes::Column::Network
                            .eq(network.to_string()),
                    )
                    .all(self.db.as_ref())
                    .await?
            }
            None => Vec::new(),
        };

        if !discovered_routes.is_empty() {
            debug!(
                "Section 6: Loading {} Traefik-discovered routes (network={})",
                discovered_routes.len(),
                discovery_network.as_deref().unwrap_or("<disabled>")
            );
        }

        for discovered in discovered_routes {
            let host = discovered.host.trim().to_ascii_lowercase();
            if host.is_empty() {
                continue;
            }

            // Precedence: any DB-driven route for this host wins, whether it
            // is an exact HTTP/TLS route, a wildcard pattern that covers the
            // host, or a legacy environment/custom-domain entry.
            let claimed_by = if routes.contains_key(&host) {
                Some("environment or custom domain route")
            } else if http_routes_map.contains_key(&host) {
                Some("HTTP custom route")
            } else if tls_routes_map.contains_key(&host) {
                Some("TLS custom route")
            } else if http_wildcards_matcher.match_domain(&host).is_some() {
                Some("HTTP wildcard custom route")
            } else if tls_wildcards_matcher.match_domain(&host).is_some() {
                Some("TLS wildcard custom route")
            } else {
                None
            };
            if let Some(owner) = claimed_by {
                warn!(
                    "Ignoring Traefik-discovered route for '{}' (container '{}'): the host \
                     already resolves to a {}. The existing route is kept.",
                    host, discovered.target_container_name, owner
                );
                continue;
            }

            // Same address construction as Temps-deployed containers, so
            // baremetal installs (where the proxy cannot resolve Docker's
            // internal DNS) use the published host port. The discovered
            // container is always local to this node — remote workers run their
            // own discovery against their own daemon.
            //
            // `None` means this deployment mode genuinely cannot reach the
            // container. Say so loudly instead of routing the host somewhere
            // else: a baremetal install has no way to reach an unpublished
            // container port, and guessing `127.0.0.1:<container port>` lands
            // on whatever unrelated service owns that port on the host.
            let Some(address) = build_discovered_backend_addr(
                &discovered.target_container_name,
                discovered.target_port,
                discovered.target_host_port,
                self.runtime_context.as_ref(),
            ) else {
                warn!(
                    "Skipping Traefik-discovered route for '{}' (container '{}', port {}): this \
                     install runs outside Docker (baremetal mode) and the container publishes no \
                     host port, so there is no address that reaches it. Publish the port on the \
                     container (e.g. `ports: - \"8080:{}\"`) or run Temps in Docker mode.",
                    host,
                    discovered.target_container_name,
                    discovered.target_port,
                    discovered.target_port
                );
                continue;
            };

            routes.insert(
                host.clone(),
                RouteInfo {
                    backend: BackendType::Upstream {
                        backends: vec![BackendEntry {
                            address: address.clone(),
                            container_id: Some(discovered.target_container_id.clone()),
                            container_name: Some(discovered.target_container_name.clone()),
                        }],
                        round_robin_counter: Arc::new(AtomicUsize::new(0)),
                    },
                    redirect_to: None,
                    status_code: None,
                    // Not a Temps deployment: no project/environment/deployment
                    // context exists for these containers by definition.
                    project: None,
                    environment: None,
                    deployment: None,
                    // TODO(security): pre-merge review finding — a discovered
                    // container's own `traefik...tls` label used to be mirrored
                    // here, which let any container on the watched network
                    // drive ACME issuance for a hostname *it* chose, burning
                    // Let's Encrypt rate limits and minting certs the operator
                    // never asked for. Hard-coded off until issuance for
                    // discovered hosts is gated on an explicit operator
                    // allowlist. `discovered.tls` is still persisted and shown
                    // in the admin API, so nothing is lost but auto-issuance.
                    cert_eligible: false,
                },
            );
            debug!(
                "Loaded Traefik-discovered route: {} -> {} (container={}, network={}, tls={})",
                host, address, discovered.target_container_name, discovered.network, discovered.tls
            );
        }

        Ok(())
    }

    /// Phase 7: custom hostnames of public status pages.
    async fn load_status_page_host_routes(
        &self,
        state: &mut RouteLoadState,
    ) -> Result<(), RouteLoadError> {
        let RouteLoadState {
            app_settings,
            routes,
            unavailable,
            http_routes_map,
            tls_routes_map,
            http_wildcards_matcher,
            tls_wildcards_matcher,
            status_page_hosts,
            ..
        } = state;

        // 7. Custom hostnames of public status pages (#1412). A hostname
        // attached to a page is reserved for it: any project route on it is
        // dropped, and it serves only while the page is published, its project
        // is not being deleted, and its own `domains` row holds a valid
        // certificate (ownership proven through ACME). A tombstone (removed
        // hostname, or deleted page/project) answers 404 too, but only while
        // nothing else claims the name: any application route wins over it.
        // The console and public ingress hostnames win over both.
        let status_bindings = load_status_page_hosts(self.db.as_ref()).await?;
        let console_host_for_status = app_settings.console_hostname();
        let ingress_host_for_status = app_settings.dedicated_public_ingress_hostname();
        let is_platform_host = |host: &str| {
            console_host_for_status.as_deref() == Some(host)
                || ingress_host_for_status.as_deref() == Some(host)
        };
        for (host, route) in status_bindings.attached {
            if is_platform_host(&host) {
                continue;
            }
            let removed = routes.remove(&host).is_some()
                | http_routes_map.remove(&host).is_some()
                | tls_routes_map.remove(&host).is_some();
            if removed {
                warn!(
                    "Ignoring project route for status-page hostname '{}' — it is attached to a status page",
                    host
                );
            }
            unavailable.exact.remove(&host);
            status_page_hosts.insert(host, route);
        }
        for host in status_bindings.tombstones {
            let claimed = is_platform_host(&host)
                || status_page_hosts.contains_key(&host)
                || routes.contains_key(&host)
                || http_routes_map.contains_key(&host)
                || tls_routes_map.contains_key(&host)
                || unavailable.exact.contains_key(&host)
                || http_wildcards_matcher.match_domain(&host).is_some()
                || tls_wildcards_matcher.match_domain(&host).is_some()
                || host
                    .split_once('.')
                    .is_some_and(|(_, parent)| routes.contains_key(&format!("*.{parent}")));
            if !claimed {
                status_page_hosts.insert(host, StatusPageHostRoute { slug: None });
            }
        }
        if !status_page_hosts.is_empty() {
            debug!(
                "Section 7: Loaded {} status-page hostnames ({} serving)",
                status_page_hosts.len(),
                status_page_hosts
                    .values()
                    .filter(|r| r.slug.is_some())
                    .count()
            );
        }

        Ok(())
    }

    /// Withdraw reserved, private and status-page hostnames from what the
    /// phases loaded, resolve gates, path routes and ingress endpoints, and
    /// build the snapshot to publish.
    async fn assemble_snapshot(
        &self,
        state: RouteLoadState,
    ) -> Result<AssembledRoutes, RouteLoadError> {
        let RouteLoadState {
            app_settings,
            mut routes,
            primary_route_hosts,
            mut sleeping_environments,
            mut unavailable,
            private_meta,
            gated_meta,
            mut http_routes_map,
            mut tls_routes_map,
            http_wildcards_matcher,
            tls_wildcards_matcher,
            status_page_hosts,
            environments_cache,
            ..
        } = state;

        // The console hostname is owned by the control plane, never by a
        // project. A route pointing it at a deployment locks the operator out
        // of the console entirely (issue #478) — the create/update API now
        // refuses such domains, but installs that already stored one would
        // otherwise stay bricked until the row is deleted over the public IP.
        // Dropping it here makes the next reload self-heal.
        if let Some(console_host) = app_settings.console_hostname() {
            let removed = routes.remove(&console_host).is_some()
                | http_routes_map.remove(&console_host).is_some()
                | tls_routes_map.remove(&console_host).is_some();
            if removed {
                warn!(
                    "Ignoring project route for reserved console hostname '{}' — the Temps console keeps it (issue #478)",
                    console_host
                );
            }
        }

        // The public ingress hostname is owned by the control plane for the
        // same reason (ADR-051): a project route on it would swallow every
        // third-party callback. The settings API refuses a hostname that is
        // already routed, so this only fires for a route created by a path
        // that bypassed that check.
        let ingress_host = app_settings.dedicated_public_ingress_hostname();
        if let Some(ingress_host) = ingress_host.as_ref() {
            let removed = routes.remove(ingress_host).is_some()
                | http_routes_map.remove(ingress_host).is_some()
                | tls_routes_map.remove(ingress_host).is_some();
            if removed {
                warn!(
                    "Ignoring project route for reserved public ingress hostname '{}' — Temps serves third-party callbacks on it (ADR-051)",
                    ingress_host
                );
            }
        }

        // ADR 064 D5: a private address is served only by the private
        // listener of the worker recorded for it. Take it out of every map
        // this proxy serves from, and out of the wake path, so neither this
        // proxy nor a public worker route ever answers it, whatever DNS says.
        // Other non-public audiences are held back the same way; `signed_in`
        // is served with its gate (below).
        let (private_routes, private_hosts) = withdraw_private_addresses(
            private_meta,
            &mut routes,
            &mut http_routes_map,
            &mut tls_routes_map,
            &mut unavailable.exact,
            &mut sleeping_environments,
        );

        // ADR 065 D8: the gate for each signed-in address. Nothing is read
        // when no address is signed in, so an install that never opts in
        // does exactly what it did before (D11).
        let gated = if gated_meta.is_empty() {
            HashMap::new()
        } else {
            let public_key = crate::gate::current_public_key(self.db.as_ref()).await?;
            let revoked = crate::gate::revoked_sessions(self.db.as_ref()).await?;
            // Cloudflare Access teams are read only when an address uses one.
            let access_teams = if gated_meta.values().any(|m| m.access.is_some()) {
                crate::access_keys::access_teams(self.db.as_ref()).await?
            } else {
                HashMap::new()
            };
            build_gate_policies(
                gated_meta,
                public_key.as_deref(),
                app_settings
                    .external_url
                    .as_deref()
                    .and_then(console_origin)
                    .map(|origin| format!("{origin}{}", temps_core::gate::AUTHORIZE_PATH))
                    .as_deref(),
                revoked,
                &access_teams,
            )
        };

        // A live route always wins over an unavailable entry for the same
        // host, whichever section recorded it, and the console hostname is
        // never reported as an application host. Lookups re-check live
        // wildcard routes; this keeps the map down to hosts that matter.
        unavailable.exact.retain(|host, _| {
            !routes.contains_key(host)
                && !http_routes_map.contains_key(host)
                && !tls_routes_map.contains_key(host)
        });
        unavailable
            .wildcard_bases
            .retain(|base, _| !routes.contains_key(&format!("*.{base}")));
        if let Some(console_host) = app_settings.console_hostname() {
            unavailable.exact.remove(&console_host);
        }
        if let Some(ingress_host) = ingress_host.as_ref() {
            unavailable.exact.remove(ingress_host);
        }

        // Build the wildcard index for legacy project/environment routes. A
        // concrete hostname resolved through a wildcard is deliberately not
        // eligible for on-demand HTTP-01 issuance: one stored wildcard may
        // cover an unbounded number of subdomains, while its certificate is
        // provisioned separately through DNS-01 and found by the TLS loader.
        let mut legacy_wildcards_matcher = WildcardMatcher::new();
        for (host, route) in routes.iter().filter(|(host, _)| {
            host.starts_with("*.")
                && !http_wildcards_matcher.contains_pattern(host)
                && !tls_wildcards_matcher.contains_pattern(host)
        }) {
            let mut wildcard_route = route.clone();
            wildcard_route.cert_eligible = false;
            legacy_wildcards_matcher.insert(host, wildcard_route);
        }

        let route_ownership = build_route_ownership_snapshot(
            routes.keys(),
            http_routes_map.keys(),
            tls_routes_map.keys(),
            app_settings
                .console_hostname()
                .into_iter()
                .chain(ingress_host.clone())
                .chain(status_page_hosts.keys().cloned()),
        );

        // Load path routes (ADR 050). If the table does not yet exist — e.g.
        // this proxy process started before the console ran the migration —
        // log the error and carry forward the previous snapshot's path routes
        // (empty if none) rather than failing the whole reload. A subsequent
        // reload after the migration will pick them up.
        let path_routes = match self.load_path_routes(&routes, &primary_route_hosts).await {
            Ok(pr) => pr,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to load path routes; keeping previous snapshot (will retry on next reload)"
                );
                self.route_snapshot.load().path_routes.clone()
            }
        };

        // Ingress endpoints (ADR-055 §11). Same carry-forward as path routes:
        // a failed read keeps the previous compiled endpoints, so an
        // enforcing environment keeps the access policy it had. Before any
        // load succeeds the previous snapshot is the default one, which
        // reports itself not loaded; the proxy refuses an enforcing
        // environment's requests against it (and against any environment
        // the kept snapshot has no endpoint for) rather than read the
        // missing policy as "no restrictions".
        let ingress = match crate::ingress::load_ingress_snapshot(
            self.db.as_ref(),
            &app_settings.rate_limiting,
        )
        .await
        {
            Ok(snapshot) => Arc::new(snapshot),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to load ingress endpoints; keeping the previous ones (will retry on next reload)"
                );
                self.route_snapshot.load().ingress.clone()
            }
        };

        let new_snapshot = Arc::new(RouteTableSnapshot {
            http_routes: http_routes_map,
            tls_routes: tls_routes_map,
            http_wildcards: http_wildcards_matcher,
            tls_wildcards: tls_wildcards_matcher,
            legacy: LegacyRouteTable {
                exact: routes,
                wildcards: legacy_wildcards_matcher,
                reserved_console_host: app_settings.console_hostname(),
                reserved_ingress_host: ingress_host,
                status_page_hosts,
                private_hosts,
            },
            ownership: route_ownership,
            path_routes,
            unavailable,
            ingress,
            private: private_routes,
            gated,
        });

        Ok(AssembledRoutes {
            snapshot: new_snapshot,
            sleeping_environments,
            environments_cache,
        })
    }
}

#[cfg(test)]
mod tests {
    //! One rebuild against a real database, with rows for each loading
    //! phase. Phases 1-3 and 6 also have focused tests in
    //! `route_table_test.rs`; these pin down phases 4, 5 and 7 and the order
    //! in which every phase lands in the same snapshot.

    use super::CachedPeerTable;
    use crate::test_utils::TestDBMockOperations;
    use sea_orm::{ActiveModelBehavior, ActiveModelTrait, ConnectionTrait, EntityTrait, Set};
    use temps_entities::{
        custom_routes, deployments, environment_domains, environments, project_custom_domains,
        settings,
    };
    use temps_test_utils::TestDatabase;

    const PREVIEW_DOMAIN: &str = "preview.example.com";
    const DISCOVERY_NETWORK: &str = "phase-test-network";

    /// Isolated database, or `None` when Docker is unavailable.
    async fn database_or_skip() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping route-loading phase test: Docker runtime unavailable: {error}");
                None
            }
            Err(error) => panic!("Could not create isolated test database: {error}"),
        }
    }

    async fn set_preview_domain(
        db: &sea_orm::DatabaseConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app_settings = temps_config::AppSettings {
            external_url: Some("https://console.example.com".to_string()),
            preview_domain: PREVIEW_DOMAIN.to_string(),
            ..Default::default()
        };
        settings::Entity::insert(settings::ActiveModel {
            id: Set(1),
            data: Set(app_settings.to_json()),
            ..settings::ActiveModel::new()
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(settings::Column::Id)
                .update_column(settings::Column::Data)
                .to_owned(),
        )
        .exec(db)
        .await?;
        Ok(())
    }

    /// A project whose production environment has subdomain `shop-production`
    /// and whose current deployment (slug `shop-abc123`) runs one container
    /// published on host port 9741.
    async fn seed_environment(
        test_db: &TestDBMockOperations,
    ) -> Result<
        (
            temps_entities::projects::Model,
            environments::Model,
            deployments::Model,
        ),
        Box<dyn std::error::Error>,
    > {
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("shop.example.com")
            .await?;
        let environment = environments::ActiveModel {
            id: Set(environment.id),
            subdomain: Set("shop-production".to_string()),
            ..Default::default()
        }
        .update(test_db.db.as_ref())
        .await?;
        let deployment = deployments::ActiveModel {
            id: Set(deployment.id),
            slug: Set("shop-abc123".to_string()),
            ..Default::default()
        }
        .update(test_db.db.as_ref())
        .await?;
        test_db
            .create_deployment_container(deployment.id, 3000, Some(9741))
            .await?;
        Ok((project, environment, deployment))
    }

    /// A published status page holding `attached` with a valid certificate,
    /// plus a detached (tombstoned) binding for `tombstone`.
    async fn seed_status_page_hosts(
        db: &sea_orm::DatabaseConnection,
        attached: &str,
        tombstone: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        db.execute_unprepared(&format!(
            "INSERT INTO status_pages (slug, title, is_published, published_at) \
             VALUES ('phase-page', 'Status', TRUE, now());
             INSERT INTO domains (domain, certificate, private_key, status, is_wildcard, \
                                  verification_method, expiration_time, created_at, updated_at) \
             VALUES ('{attached}', 'pem', 'key', 'active', FALSE, 'http-01', \
                     now() + interval '30 days', now(), now());
             INSERT INTO status_page_domains (status_page_id, hostname, domain_id) \
             SELECT sp.id, '{attached}', d.id FROM status_pages sp, domains d \
             WHERE sp.slug = 'phase-page' AND d.domain = '{attached}';
             INSERT INTO status_page_domains (status_page_id, hostname, detached_at) \
             VALUES (NULL, '{tombstone}', now());"
        ))
        .await?;
        Ok(())
    }

    fn deployment_of(table: &CachedPeerTable, host: &str) -> Option<i32> {
        table
            .get_route(host)
            .and_then(|route| route.deployment.as_ref().map(|d| d.id))
    }

    /// Phase 4 serves an environment's subdomain as-is, under the preview
    /// domain, and under the internal `*.temps.local` name.
    #[tokio::test]
    async fn environment_phase_routes_subdomain_preview_and_internal_hosts(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        set_preview_domain(test_db.db.as_ref()).await?;
        let (project, environment, deployment) = seed_environment(&test_db).await?;

        let table = CachedPeerTable::new(test_db.db.clone());
        table.load_routes().await?;

        let preview_host = format!("shop-production.{PREVIEW_DOMAIN}");
        let internal_host = format!("{}.{}.temps.local", environment.slug, project.slug);
        for host in [
            "shop-production",
            preview_host.as_str(),
            internal_host.as_str(),
        ] {
            let route = table
                .get_route(host)
                .unwrap_or_else(|| panic!("phase 4 must route {host}"));
            assert_eq!(route.get_backend_addr(), "127.0.0.1:9741", "{host}");
            assert_eq!(
                route.environment.as_ref().map(|e| e.id),
                Some(environment.id),
                "{host}"
            );
            assert_eq!(deployment_of(&table, host), Some(deployment.id), "{host}");
        }
        assert!(table
            .get_route(&preview_host)
            .is_some_and(|r| r.cert_eligible));
        assert!(
            table
                .get_route(&internal_host)
                .is_some_and(|r| !r.cert_eligible),
            "an internal temps.local name is never on-demand certable"
        );

        test_db.cleanup().await?;
        Ok(())
    }

    /// Phase 5 gives every active deployment its fallback hostname, which is
    /// never on-demand certable.
    #[tokio::test]
    async fn active_deployment_phase_routes_the_deployment_fallback_hostname(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        set_preview_domain(test_db.db.as_ref()).await?;
        let (project, _environment, deployment) = seed_environment(&test_db).await?;

        let table = CachedPeerTable::new(test_db.db.clone());
        table.load_routes().await?;

        let fallback = format!("shop-abc123.{PREVIEW_DOMAIN}");
        let route = table
            .get_route(&fallback)
            .expect("phase 5 must route the deployment fallback hostname");
        assert_eq!(route.get_backend_addr(), "127.0.0.1:9741");
        assert_eq!(route.project.as_ref().map(|p| p.id), Some(project.id));
        assert_eq!(deployment_of(&table, &fallback), Some(deployment.id));
        assert!(!route.cert_eligible);

        test_db.cleanup().await?;
        Ok(())
    }

    /// Phase 7 reserves a hostname attached to a status page, dropping the
    /// project route an earlier phase put on it, and records a tombstone that
    /// nothing else claims.
    #[tokio::test]
    async fn status_page_phase_reserves_attached_hostnames_over_project_routes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        seed_environment(&test_db).await?;
        custom_routes::ActiveModel {
            domain: Set("status.example.com".to_string()),
            host: Set("localhost".to_string()),
            port: Set(8080),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;
        seed_status_page_hosts(
            test_db.db.as_ref(),
            "status.example.com",
            "old-status.example.com",
        )
        .await?;

        let table = CachedPeerTable::new(test_db.db.clone());
        table.load_routes().await?;

        assert!(
            table.get_route("status.example.com").is_none(),
            "a status-page hostname drops the project route on it"
        );
        let attached = table
            .status_page_host("status.example.com")
            .expect("an attached hostname is served by its status page");
        assert_eq!(attached.slug.as_deref(), Some("phase-page"));
        let tombstone = table
            .status_page_host("old-status.example.com")
            .expect("an unclaimed tombstone answers 404");
        assert_eq!(tombstone.slug, None);

        test_db.cleanup().await?;
        Ok(())
    }

    /// One rebuild with rows for all seven phases: each phase's hosts land in
    /// the same snapshot.
    #[tokio::test]
    async fn every_loading_phase_contributes_to_one_rebuild(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        let db = test_db.db.as_ref();
        set_preview_domain(db).await?;
        let (project, environment, deployment) = seed_environment(&test_db).await?;

        // Phase 1: an environment domain.
        environment_domains::ActiveModel {
            domain: Set("env-domain.example.com".to_string()),
            environment_id: Set(environment.id),
            ..Default::default()
        }
        .insert(db)
        .await?;
        // Phase 2: an operator custom route.
        custom_routes::ActiveModel {
            domain: Set("operator.example.com".to_string()),
            host: Set("10.0.0.9".to_string()),
            port: Set(8081),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(db)
        .await?;
        // Phase 3: a project custom domain.
        project_custom_domains::ActiveModel {
            domain: Set("custom.example.com".to_string()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            status: Set("active".to_string()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        // Phase 6: a Traefik-discovered route on the watched network.
        temps_entities::traefik_discovered_routes::ActiveModel {
            host: Set("discovered.example.com".to_string()),
            router_name: Set("app".to_string()),
            target_container_id: Set("whoami-id".to_string()),
            target_container_name: Set("whoami".to_string()),
            target_port: Set(8000),
            target_host_port: Set(Some(18000)),
            network: Set(DISCOVERY_NETWORK.to_string()),
            tls: Set(false),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(db)
        .await?;
        // Phase 7: a status-page hostname and a tombstone.
        seed_status_page_hosts(db, "status.example.com", "old.example.com").await?;

        let table = CachedPeerTable::new(test_db.db.clone());
        table.set_traefik_discovery_network(Some(DISCOVERY_NETWORK.to_string()));
        let sleeping = table.load_routes().await?;
        assert!(sleeping.is_empty());

        // Phases 1, 3, 4 and 5 route to the environment's deployment.
        for host in [
            "env-domain.example.com".to_string(),
            "custom.example.com".to_string(),
            "shop-production".to_string(),
            format!("shop-production.{PREVIEW_DOMAIN}"),
            format!("shop-abc123.{PREVIEW_DOMAIN}"),
        ] {
            assert_eq!(deployment_of(&table, &host), Some(deployment.id), "{host}");
        }
        // Phase 2.
        assert_eq!(
            table
                .get_route("operator.example.com")
                .map(|r| r.get_backend_addr()),
            Some("10.0.0.9:8081".to_string())
        );
        // Phase 6.
        let discovered = table
            .get_route("discovered.example.com")
            .expect("phase 6 must route the discovered host");
        assert!(discovered.deployment.is_none());
        // Phase 7.
        assert_eq!(
            table
                .status_page_host("status.example.com")
                .and_then(|r| r.slug)
                .as_deref(),
            Some("phase-page")
        );
        assert!(table.status_page_host("old.example.com").is_some());
        assert!(table.has_loaded());

        test_db.cleanup().await?;
        Ok(())
    }
}
