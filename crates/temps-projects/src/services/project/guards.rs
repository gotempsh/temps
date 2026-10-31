// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Authorization guards: reserved-slug claims, Docker-socket grants and
//! tenant resource ceilings.

use super::*;

impl ProjectService {
    /// Override the host Docker socket grant this service answers from.
    ///
    /// Exists for tests: production always uses the process-wide grant read
    /// from the environment at startup, which is deliberately un-settable at
    /// runtime.
    #[cfg(test)]
    pub(super) fn with_docker_socket_grant(
        mut self,
        grant: temps_core::docker_socket_grant::DockerSocketGrant,
    ) -> Self {
        self.docker_socket_grant = grant;
        self
    }

    /// Refuse a move of a slug this host grants host Docker access to
    /// (ADR 045), unless the caller is an instance admin.
    ///
    /// Both directions are guarded, and for different reasons.
    /// [`ReservedSlugChange::Claim`] hands a project host root on every host
    /// that grants the slug. [`ReservedSlugChange::Release`] takes it away
    /// from a project that has it — silently breaking an operator-owned
    /// infrastructure service — and frees the slug for the next project
    /// created, which would inherit the access. Guarding only the claim would
    /// leave "rename it away, then create your own" open to any project
    /// writer.
    ///
    /// `project_id` is `Some` for a rename and `None` for a create; it only
    /// enriches the rejection log.
    ///
    /// Answers from this process's own grant, exactly like the deployer's bind
    /// decision and the capability response, so the three can never disagree.
    /// Two questions, in this order: *may* this caller move the slug, and is
    /// their session *recent enough* to. The order is the security property —
    /// a project writer gets the 403 that names ADR 045, never an MFA prompt
    /// for an operation they still would not be allowed to perform.
    ///
    /// The step-up lives here rather than in the three handlers so it cannot
    /// be forgotten by the fourth: every path that can move a slug already
    /// funnels through this one guard.
    ///
    /// This guarantees the *result* is correct -- no project is ever
    /// created or renamed onto a granted slug without authority and step-up
    /// -- but says nothing about side effects a caller performs *before*
    /// reaching it. A template deploy in fork mode was found doing exactly
    /// that: creating and pushing to an upstream repository before this
    /// guard ever ran, so a 428 here left a real external repository with
    /// no Temps project. [`Self::preflight_guard_reserved_slug`] exists for
    /// callers with an irreversible step between planning a slug and
    /// creating the project -- check it first, then still go through this
    /// guard at creation, since a slug can change between the two calls
    /// (another create/rename racing on the same name) and only this one is
    /// authoritative.
    pub(super) async fn guard_reserved_slug(
        &self,
        slug: &str,
        caller: &SlugClaimCaller<'_>,
        project_id: Option<i32>,
        change: ReservedSlugChange,
    ) -> Result<(), ProjectError> {
        Self::guard_reserved_slug_against(
            &self.docker_socket_grant,
            slug,
            caller.authority,
            project_id,
            change,
        )?;
        self.require_slug_claim_step_up(slug, caller, project_id, change)
            .await
    }

    /// Check authority and step-up for creating a project with `slug`,
    /// before a caller does anything irreversible on the strength of "this
    /// creation will probably succeed".
    ///
    /// A template deploy in fork mode creates and pushes to a real upstream
    /// repository before it ever calls [`Self::create_project_as`], using a
    /// slug it already planned with [`Self::plan_project_slug`]. Without
    /// this check, a refusal at creation time (403, or a 428 step-up
    /// challenge) arrives *after* that external side effect, leaving a real
    /// repository with no Temps project — and, for the 428 case, a retry
    /// that resends the same repository name fails with "already exists"
    /// instead of completing.
    ///
    /// This does not replace the check inside [`Self::create_project_as`]:
    /// the planned slug can change between this call and creation (a
    /// concurrent create or rename claiming the same name), so only the
    /// creation-time guard is authoritative. This is purely fail-fast for
    /// the common case, at the cost of one extra check.
    pub async fn preflight_guard_reserved_slug(
        &self,
        slug: &str,
        caller: &SlugClaimCaller<'_>,
    ) -> Result<(), ProjectError> {
        self.guard_reserved_slug(slug, caller, None, ReservedSlugChange::Claim)
            .await
    }

    /// The step-up half of [`Self::guard_reserved_slug`], reached only once
    /// the authority check has already said yes.
    ///
    /// Returns `Ok(())` immediately for a slug this host does not reserve, so
    /// an ordinary project create or rename never sees an MFA prompt.
    async fn require_slug_claim_step_up(
        &self,
        slug: &str,
        caller: &SlugClaimCaller<'_>,
        project_id: Option<i32>,
        change: ReservedSlugChange,
    ) -> Result<(), ProjectError> {
        if !temps_core::docker_socket_grant::slug_is_reserved(&self.docker_socket_grant, slug) {
            return Ok(());
        }
        let Some(step_up) = caller.step_up.as_ref() else {
            // Admin authority with nothing to challenge. No production path
            // constructs that — `SlugClaimCaller::from_request` always carries
            // the authorizer, and `project_writer()` can never reach this line
            // because the authority check above refuses it first. Reaching it
            // means a future caller asserted an authority out of band, so
            // refuse rather than silently skip the challenge.
            warn!(
                slug = %slug,
                project_id = ?project_id,
                change = ?change,
                "Refused a reserved-slug change from a caller claiming admin authority with no \
                 session to challenge (ADR 045)"
            );
            return Err(ProjectError::DockerSocketSlugReserved {
                slug: slug.to_string(),
                change,
            });
        };
        // `project_id` is `None` on create, where there is no row yet to name.
        // `0` is the "not yet persisted" sentinel in the action identity; it
        // only ever appears in the challenge's `action` payload and the
        // step-up audit trail, never as a lookup key.
        let project_id = project_id.unwrap_or(0);
        let action = match change {
            ReservedSlugChange::Claim => {
                temps_core::SensitiveAction::ClaimDockerSocketSlug { project_id }
            }
            ReservedSlugChange::Release => {
                temps_core::SensitiveAction::ReleaseDockerSocketSlug { project_id }
            }
        };
        temps_auth::require_sensitive_action(step_up.authorizer, step_up.auth, action)
            .await
            .map_err(|problem| ProjectError::SlugClaimStepUpRequired {
                problem: Box::new(problem),
            })
    }

    /// [`Self::guard_reserved_slug`] with the grant injected.
    ///
    /// Pure, mirroring `temps_deployer::docker::docker_socket_bind_for`: the
    /// process grant is a `OnceLock` frozen at first use, so the rule would
    /// otherwise be untestable without mutating process-global environment
    /// state from parallel tests.
    pub(super) fn guard_reserved_slug_against(
        grant: &temps_core::docker_socket_grant::DockerSocketGrant,
        slug: &str,
        authority: SlugClaimAuthority,
        project_id: Option<i32>,
        change: ReservedSlugChange,
    ) -> Result<(), ProjectError> {
        if !temps_core::docker_socket_grant::slug_is_reserved(grant, slug)
            || authority.may_claim_reserved_slug()
        {
            return Ok(());
        }
        // No audit event covers "a write that never happened", and this one
        // matters: it is an attempt to move host root between projects. Logged
        // with everything the service knows, so an operator reviewing host
        // logs can see who tried and on which project.
        warn!(
            slug = %slug,
            project_id = ?project_id,
            change = ?change,
            env = temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV,
            "{}",
            match change {
                ReservedSlugChange::Claim =>
                    "Refused a non-admin claim on a slug this host grants host Docker access to \
                     (ADR 045)",
                ReservedSlugChange::Release =>
                    "Refused a non-admin rename away from a slug this host grants host Docker \
                     access to (ADR 045)",
            }
        );
        Err(ProjectError::DockerSocketSlugReserved {
            slug: slug.to_string(),
            change,
        })
    }

    /// Refuse a deployment of a project this control plane declares as
    /// requiring the host Docker socket (ADR 045), unless the caller is an
    /// instance admin (or Temps itself).
    ///
    /// The slug guard decides who may *point a project at* host root; this one
    /// decides who may *run code as* host root, and they are different
    /// principals: a granted project created by an admin carries no
    /// restrictive access grants, so without this any holder of
    /// `ProjectsWrite`/`DeploymentsCreate` on it could deploy their own image
    /// and command into a container that receives `/var/run/docker.sock`.
    pub(super) fn guard_granted_project_deploy(
        &self,
        project_slug: &str,
        caller: DeployCaller,
    ) -> Result<(), ProjectError> {
        Self::guard_granted_project_deploy_against(&self.docker_socket_grant, project_slug, caller)
    }

    /// [`Self::guard_granted_project_deploy`] with the grant injected, so the
    /// rule is testable without the process-wide `OnceLock`.
    fn guard_granted_project_deploy_against(
        grant: &temps_core::docker_socket_grant::DockerSocketGrant,
        project_slug: &str,
        caller: DeployCaller,
    ) -> Result<(), ProjectError> {
        if !temps_core::docker_socket_grant::deploy_requires_instance_admin(
            grant,
            project_slug,
            caller,
        ) {
            return Ok(());
        }
        warn!(
            slug = %project_slug,
            env = temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV,
            "Refused a non-admin deployment of a project that holds host Docker access (ADR 045)"
        );
        Err(ProjectError::DockerSocketDeployRequiresAdmin {
            slug: project_slug.to_string(),
        })
    }

    /// Refuse a write that decides what a project holding host Docker access
    /// (ADR 045) *runs*, unless the caller is an instance admin.
    ///
    /// The third of the three ADR-045 guards, and the one that closes the
    /// two-step version of the attack the other two refuse in one step.
    /// `guard_reserved_slug` decides who may point a project at host root;
    /// `guard_granted_project_deploy` decides who may run code as host root
    /// right now. Neither covers a project writer who *plants* the input and
    /// lets somebody else's deployment execute it: the persisted runtime
    /// command becomes the default for every deploy that does not override it
    /// (including an admin's), and the source-repository fields decide what a
    /// git push builds and runs — and a git push carries no authenticated
    /// principal to gate at deploy time at all.
    ///
    /// `field` is the caller-facing name of what they tried to change; it
    /// reaches the API message, so it must read as a noun phrase.
    pub(super) fn guard_granted_project_write(
        &self,
        project_slug: &str,
        field: &str,
        caller: DeployCaller,
    ) -> Result<(), ProjectError> {
        Self::guard_granted_project_write_against(
            &self.docker_socket_grant,
            project_slug,
            field,
            caller,
        )
    }

    /// [`Self::guard_granted_project_write`] with the grant injected, so the
    /// rule is testable without the process-wide `OnceLock`.
    pub(super) fn guard_granted_project_write_against(
        grant: &temps_core::docker_socket_grant::DockerSocketGrant,
        project_slug: &str,
        field: &str,
        caller: DeployCaller,
    ) -> Result<(), ProjectError> {
        if !temps_core::docker_socket_grant::deploy_requires_instance_admin(
            grant,
            project_slug,
            caller,
        ) {
            return Ok(());
        }
        warn!(
            slug = %project_slug,
            field = %field,
            env = temps_core::docker_socket_grant::DOCKER_SOCKET_PROJECTS_ENV,
            "Refused a non-admin write to a setting that decides what a project holding host \
             Docker access runs (ADR 045)"
        );
        Err(ProjectError::DockerSocketWriteRequiresAdmin {
            slug: project_slug.to_string(),
            field: field.to_string(),
        })
    }

    /// Where this project is granted host Docker access (ADR 045).
    ///
    /// Answers from two sources, both authoritative for the host they describe:
    /// this control plane's own process-wide grant, and each worker node's
    /// advertised grant from its last heartbeat. Nodes of any status are
    /// counted — an offline node still *grants* the project, and reporting
    /// otherwise would send the operator to change a variable that is already
    /// correct. Whether a granted project can be placed *right now* is the
    /// scheduler's answer, not this one.
    ///
    /// Only ever computed for a single project's detail view, never per row of
    /// a list: it is one extra query, and the list endpoints must stay cheap.
    ///
    /// The match is done by Postgres, not by Rust: this runs on every
    /// project-detail request, and loading every `nodes` row (each carrying a
    /// full heartbeat `capacity` blob and a labels blob) to then discard
    /// almost all of them costs memory proportional to the fleet for an answer
    /// that is usually one name or none. JSONB containment on the array the
    /// agent advertises does the same test in the index-able place, and only
    /// the node *name* is selected, since that is all the capability carries.
    pub async fn docker_socket_capability(
        &self,
        slug: &str,
    ) -> Result<DockerSocketCapability, ProjectError> {
        // `capacity->'docker_socket_projects' @> '["<slug>"]'` is true only for
        // an exact element match, which is the same exact-match rule the
        // deployer's bind and the reservation guard use — a near miss must
        // fail closed rather than widen the grant. The slug is bound as a
        // parameter (never interpolated), and a node whose capacity has no
        // such key, or a non-array there, simply does not match — the tolerant
        // behaviour `capacity_grants` already has for an older agent.
        let granting_node_names: Vec<String> = temps_entities::nodes::Entity::find()
            .select_only()
            .column(temps_entities::nodes::Column::Name)
            .filter(sea_orm::sea_query::Expr::cust_with_values(
                format!(
                    "\"capacity\" -> '{}' @> $1::jsonb",
                    temps_core::docker_socket_grant::NODE_CAPACITY_KEY
                ),
                [sea_orm::Value::from(
                    serde_json::Value::from(vec![slug.to_string()]).to_string(),
                )],
            ))
            .order_by_asc(temps_entities::nodes::Column::Name)
            .into_tuple::<String>()
            .all(self.db.as_ref())
            .await?;

        Ok(DockerSocketCapability::evaluate(
            slug,
            // The control plane's declaration is the gate: a node advertising
            // a slug this process never declared is a misconfigured (or
            // hostile) node, not a grant.
            self.docker_socket_grant.declares(slug),
            granting_node_names,
        ))
    }

    /// Reject `deployment_config` if it violates `app_settings`'s tenant
    /// resource ceilings; a no-op when no ceiling is configured.
    ///
    /// Pulled out of the two call sites below so the error-mapping (join the
    /// violations into one `InvalidInput`) has a single place to test without
    /// standing up a full `ProjectService` — the ceiling logic itself is
    /// covered independently on [`temps_entities::deployment_config::DeploymentConfig::check_against_tenant_ceilings`].
    pub(super) fn enforce_tenant_ceilings(
        deployment_config: &temps_entities::deployment_config::DeploymentConfig,
        app_settings: &temps_core::AppSettings,
    ) -> Result<(), ProjectError> {
        deployment_config
            .check_against_tenant_ceilings(&app_settings.tenant_resource_ceilings)
            .map_err(|violations| ProjectError::InvalidInput(violations.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── tenant resource ceilings ─────────────────────────────────────────

    /// The default ceilings are unenforced, so a project config that uses
    /// every `0 = unlimited` sentinel must pass unchanged — this is what
    /// "an upgrade changes nothing" means at the service layer.
    #[test]
    fn enforce_tenant_ceilings_is_a_noop_with_default_settings() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            memory_limit: Some(0),
            max_concurrent_connections: Some(0),
            request_timeout_seconds: Some(0),
            ..Default::default()
        };
        let app_settings = temps_core::AppSettings::default();

        assert!(ProjectService::enforce_tenant_ceilings(&config, &app_settings).is_ok());
    }

    /// A configured ceiling rejects a violating config with an
    /// `InvalidInput` whose message names every violation, not just the
    /// first — the caller joins them with "; " for a single round trip.
    #[test]
    fn enforce_tenant_ceilings_rejects_violations_with_a_joined_message() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            memory_limit: Some(8192),
            max_concurrent_connections: Some(500),
            ..Default::default()
        };
        let app_settings = temps_core::AppSettings {
            tenant_resource_ceilings: temps_core::TenantResourceCeilings {
                max_memory_limit_mb: 4096,
                max_concurrent_connections: 200,
                allow_unlimited_request_timeouts: true,
            },
            ..Default::default()
        };

        let error = ProjectService::enforce_tenant_ceilings(&config, &app_settings)
            .expect_err("values above the ceiling must be rejected");
        match error {
            ProjectError::InvalidInput(msg) => {
                assert!(msg.contains("4096"), "got: {msg}");
                assert!(msg.contains("200"), "got: {msg}");
                assert!(msg.contains(';'), "expected violations joined, got: {msg}");
            }
            other => panic!("expected InvalidInput, got: {other:?}"),
        }
    }

    /// A config within every configured ceiling is accepted.
    #[test]
    fn enforce_tenant_ceilings_accepts_values_within_the_ceiling() {
        let config = temps_entities::deployment_config::DeploymentConfig {
            memory_limit: Some(2048),
            max_concurrent_connections: Some(100),
            ..Default::default()
        };
        let app_settings = temps_core::AppSettings {
            tenant_resource_ceilings: temps_core::TenantResourceCeilings {
                max_memory_limit_mb: 4096,
                max_concurrent_connections: 200,
                allow_unlimited_request_timeouts: true,
            },
            ..Default::default()
        };

        assert!(ProjectService::enforce_tenant_ceilings(&config, &app_settings).is_ok());
    }
}
