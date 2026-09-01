// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin entrypoint for teams + project-scoped RBAC.
//!
//! Registration order matters: this plugin registers the
//! `Arc<dyn ProjectAccessChecker>` that every other plugin's
//! `configure_routes` resolves via
//! `context.get_service::<dyn temps_core::ProjectAccessChecker>()`. Since
//! `register_services` runs for **all** plugins before **any**
//! `configure_routes`, the checker is guaranteed to be in the registry by
//! the time project-scoped plugins build their `AppState` — regardless of
//! the order plugins are registered in.
//!
//! Until something implements `ProjectAccessChecker`, every
//! `project_access_guard!` call site across the platform is a no-op. This
//! plugin is what makes them real.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use tracing::debug;
use utoipa::openapi::OpenApi;
use utoipa::OpenApi as OpenApiTrait;

use crate::checker::TeamProjectAccessChecker;
use crate::handlers::{router, TeamsApiDoc, TeamsAppState};
use crate::service::{DefaultTeamService, TeamService};

pub struct TeamsPlugin;

impl TeamsPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TeamsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for TeamsPlugin {
    fn name(&self) -> &'static str {
        "teams"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let db = context.require_service::<DatabaseConnection>();

            // Owns the caches and the DB handle for the authorization
            // queries. Its optional membership resolver is attached later,
            // in `configure_routes` — see there for why.
            let checker = Arc::new(TeamProjectAccessChecker::new(db.clone()));

            // The team service holds an Arc to the concrete checker so it
            // can invalidate the caches after writes.
            let team_service: Arc<dyn TeamService> =
                Arc::new(DefaultTeamService::new(db.clone()).with_checker(checker.clone()));
            context.register_service(team_service.clone());

            let audit = context.require_service::<dyn temps_core::AuditLogger>();

            let sensitive_action_authorizer =
                context.require_service::<dyn temps_core::SensitiveActionAuthorizer>();

            let app_state = Arc::new(TeamsAppState::new(
                team_service,
                audit,
                checker.clone(),
                sensitive_action_authorizer,
            ));
            context.register_service(app_state);

            // Registering this activates `project_access_guard!` and
            // `project_permission_guard!` on every project-scoped handler
            // in the platform.
            let checker_trait: Arc<dyn temps_core::ProjectAccessChecker> = checker;
            context.register_service(checker_trait);

            debug!("teams: services registered (ProjectAccessChecker active)");
            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // Rebind the authorizer here rather than trust the one captured in
        // `register_services`: an EE/custom `SensitiveActionAuthorizer`
        // (e.g. one that denies unenrolled principals) may be registered by
        // a plugin later in registration order, and `register_services`
        // last-write-wins semantics mean the earliest-registered instance
        // otherwise wins silently. `configure_routes` runs only after every
        // plugin's `register_services` has completed, so re-resolving here
        // always sees the final policy — same pattern as AuthPlugin's
        // `with_sensitive_action_authorizer`.
        let app_state = Arc::new(TeamsAppState {
            sensitive_action_authorizer: context
                .require_service::<dyn temps_core::SensitiveActionAuthorizer>(),
            ..(*context.require_service::<TeamsAppState>()).clone()
        });

        // Attach the optional membership resolver here rather than in
        // `register_services`: it's registered by a different plugin, and
        // plugin registration order isn't guaranteed. Every plugin's
        // services are registered before any plugin configures routes, so
        // this is the first point at which a resolver is guaranteed
        // visible however the plugins happen to be ordered.
        app_state.checker.set_membership_resolver(
            context.get_service::<dyn temps_core::MembershipPermissionResolver>(),
        );

        Some(PluginRoutes::new(router(app_state)))
    }

    fn openapi_schema(&self) -> Option<OpenApi> {
        Some(<TeamsApiDoc as OpenApiTrait>::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_stable() {
        assert_eq!(TeamsPlugin::new().name(), "teams");
    }
}
