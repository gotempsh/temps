// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 2: register the services every plugin may resolve, before any
//! plugin registers its own.

use super::*;

/// What [`register_core_services`] registers. Owned, so each service keeps
/// its own `Arc` clone.
pub(super) struct CoreServices {
    pub(super) db: Arc<DbConnection>,
    pub(super) config: Arc<ServerConfig>,
    pub(super) encryption_service: Arc<EncryptionService>,
    pub(super) cookie_crypto: Arc<CookieCrypto>,
    pub(super) docker_handle: Arc<temps_core::DockerHandle>,
    pub(super) docker_available: bool,
    pub(super) profile: crate::commands::serve::ServeProfile,
    pub(super) route_table: Arc<temps_proxy::CachedPeerTable>,
    pub(super) retention_resolver_slot: Arc<temps_core::RetentionResolverSlot>,
    pub(super) project_ip_gate_slot: Arc<temps_core::ProjectIpGateSlot>,
    pub(super) request_policy_gate_slot: Arc<temps_core::RequestPolicyGateSlot>,
    pub(super) overlay_dns_slot: temps_dns::OverlayDnsSlot,
    pub(super) dns_policy_slot: temps_dns::DnsPolicySlot,
    pub(super) update_status: Arc<temps_core::UpdateStatusSlot>,
    pub(super) self_updater: Arc<crate::commands::serve::self_update::BinarySelfUpdater>,
    pub(super) traefik_discovery: Arc<temps_deployer::traefik_discovery::TraefikDiscoveryHandle>,
    pub(super) additional_templates: Vec<PathBuf>,
    pub(super) on_demand_waker: Option<Arc<dyn temps_core::OnDemandWaker>>,
    pub(super) activator: Arc<temps_workload_activator::Activator>,
    pub(super) local_drivers: Arc<temps_workload_plane::LocalDrivers>,
}

/// The core services later steps use directly (the agent-facing routes and
/// the signed-in gate's authorize page).
pub(super) struct CoreServiceHandles {
    pub(super) gate_service: Arc<temps_routes::gate::GateService>,
    pub(super) access_key_service: Arc<temps_routes::access_keys::AccessKeyService>,
}

/// Register the core services (database, crypto, Docker handle, the shared
/// slots owned by `serve`, the route table, templates, the workload plane)
/// that plugins resolve during their own registration.
pub(super) async fn register_core_services(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    services: CoreServices,
) -> anyhow::Result<CoreServiceHandles> {
    let CoreServices {
        db,
        config,
        encryption_service,
        cookie_crypto,
        docker_handle,
        docker_available,
        profile,
        route_table,
        retention_resolver_slot,
        project_ip_gate_slot,
        request_policy_gate_slot,
        overlay_dns_slot,
        dns_policy_slot,
        update_status,
        self_updater,
        traefik_discovery,
        additional_templates,
        on_demand_waker,
        activator,
        local_drivers,
    } = services;

    // Register core services that plugins can access
    service_context.register_service(db.clone());
    service_context.register_service(encryption_service.clone());
    service_context.register_service(cookie_crypto.clone());
    // The Docker client handle is ALWAYS registered; the daemon behind it is
    // not always there. Plugins resolve the handle with `require_service` (it
    // genuinely always exists) and then make the *daemon* optional at the
    // point of use, so a control plane with no socket never panics inside
    // `require_service::<bollard::Docker>()`.
    service_context.register_service(docker_handle.clone());
    // The raw client stays registered too, but only when one exists, so any
    // consumer that has not been migrated to the handle fails the boot-time
    // `verify_required_services` check with a readable error naming itself
    // rather than panicking half-way through initialization.
    if let Some(client) = docker_handle.cloned() {
        service_context.register_service(client);
    }
    // The single boot-time answer to "may this process run workloads?".
    // Registered before any plugin runs so `register_services` can consult it.
    let local_workload_policy = Arc::new(if profile.local_workloads_enabled() {
        temps_core::LocalWorkloadPolicy::full(docker_available)
    } else {
        temps_core::LocalWorkloadPolicy::control_plane(docker_available)
    });
    service_context.register_service(local_workload_policy.clone());
    // Pre-registered here (rather than left solely to AuthPlugin, which also
    // registers an equivalent instance) because TeamsPlugin, GitPlugin,
    // DomainsPlugin, and DeploymentsPlugin all gate sensitive mutations via
    // `require_sensitive_action` and are registered before AuthPlugin in the
    // ordered list below. Only depends on `db`, so it's safe to construct
    // this early.
    let sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer> = Arc::new(
        temps_auth::DefaultSensitiveActionAuthorizer::new(db.clone()),
    );
    service_context.register_service(sensitive_action_authorizer);
    // Background DNS mutation is fail-closed until an optional policy plugin
    // claims this slot. DomainsPlugin captures the slot before later plugins
    // register, so the indirection must exist before plugin initialization.
    let dns_automation_gate_slot = Arc::new(temps_core::DnsAutomationGateSlot::new());
    service_context.register_service(dns_automation_gate_slot);
    // Pre-registered before any plugin runs so ProxyPlugin uses this exact
    // slot instance instead of creating its own — see the field doc on
    // `ConsoleApiParams::retention_resolver_slot`.
    service_context.register_service(retention_resolver_slot.clone());
    // Same pre-registration reasoning as retention_resolver_slot above —
    // see the field doc on `ConsoleApiParams::project_ip_gate_slot` (ADR
    // 0022) for why this is flagged for security review rather than a
    // routine addition.
    service_context.register_service(project_ip_gate_slot.clone());
    service_context.register_service(request_policy_gate_slot.clone());
    service_context.register_service(overlay_dns_slot);
    service_context.register_service(dns_policy_slot);
    // Update-notifier slot: the background loop in serve/mod.rs writes into
    // it; ConfigPlugin's `GET /settings/update-status` reads it so the web
    // console can render the upgrade banner.
    service_context.register_service(update_status.clone());
    // Registered behind the trait so temps-config depends only on the
    // temps-core contract, never on the CLI crate that implements it.
    service_context.register_service(self_updater.clone() as Arc<dyn temps_core::SelfUpdater>);
    // The version this binary was built as, so the commands the server
    // generates for a new worker (the pairing command) install the release
    // it runs, as the console's Add worker panel does.
    service_context.register_service(Arc::new(temps_core::worker_install::PlaneBuild::new(env!(
        "TEMPS_VERSION"
    ))));
    // Traefik label discovery status, resolved in serve/mod.rs. Pre-registered
    // before any plugin runs so DeploymentsPlugin's `/traefik-discovery/*`
    // handlers report this process's real state instead of falling back to a
    // handle rebuilt from the environment.
    service_context.register_service(traefik_discovery.clone());

    // Register the shared route table (created in serve/mod.rs)
    // This is used by analytics-events and other plugins that need to resolve hosts
    // Note: Route table listener is started in serve/mod.rs to avoid duplicate listeners
    service_context.register_service(route_table.clone());
    service_context.register_service(
        route_table.clone() as Arc<dyn temps_core::route_table::RouteTableRefresher>
    );
    // The signed-in gate (ADR 065 D8): issues sign-in codes for the console's
    // authorize endpoint, redeems them for agents, revokes sessions.
    let gate_service = Arc::new(temps_routes::gate::GateService::new(
        db.clone(),
        encryption_service.clone(),
    ));
    service_context.register_service(gate_service.clone());
    // Cloudflare Access signing keys (ADR 065 D9): fetched again every hour
    // for the tokens in use, and when an agent meets an unknown key. The
    // hourly loop reads nothing and fetches nothing while no address uses
    // Access.
    let access_key_service = Arc::new(temps_routes::access_keys::AccessKeyService::new(db.clone()));
    service_context.register_service(access_key_service.clone());
    tokio::spawn(access_key_service.clone().run_hourly());

    // Register TemplateService - provides project templates from YAML configuration
    // Bundled templates are loaded automatically; external file in data_dir can override them
    let templates_override_path = config.data_dir.join("templates.yaml");
    let template_service = Arc::new(TemplateService::new(Some(templates_override_path))?);

    // Load additional template files if specified. `load_additional` warns and
    // skips individual templates with unknown presets or other validation
    // errors; it only returns `Err` when the file itself is missing or
    // unparsable, which is always a boot-time configuration mistake.
    for additional_path in &additional_templates {
        info!("Loading additional templates from {:?}", additional_path);
        if let Err(e) = template_service.load_additional(additional_path).await {
            return Err(anyhow::anyhow!(
                "Failed to load additional templates from {:?}\n\n\
                Error: {}\n\n\
                Check that the file exists and contains valid YAML. Templates with \
                unrecognised presets are skipped with a warning; valid presets are: {}",
                additional_path,
                e,
                temps_presets::templates::VALID_PRESETS.join(", ")
            ));
        }
    }

    service_context.register_service(template_service);

    // Register OnDemandWaker so environment wake/sleep endpoints can manage containers
    if let Some(waker) = on_demand_waker {
        service_context.register_service(waker as Arc<dyn temps_core::OnDemandWaker>);
        debug!("Registered OnDemandWaker for environment wake/sleep endpoints");
    }

    // ADR-055 phase 10: the activator, the plane handle the deployments
    // plugin fills once the plane starts, and the extra runtimes it adds.
    service_context.register_service(activator);
    service_context.register_service(Arc::new(temps_workload_plane::SharedPlane::new()));
    service_context.register_service(local_drivers);

    Ok(CoreServiceHandles {
        gate_service,
        access_key_service,
    })
}
