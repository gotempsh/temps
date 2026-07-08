# ADR 028: Project-Scoped RBAC Enforcement for Human Sessions

- **Status:** Proposed
- **Date:** 2026-07-08
- **Deciders:** David
- **Supersedes:** —
- **Related:** ADR 009 (secrets-manager resolver, reuses the same optional-extension-point pattern); this ADR is also a prerequisite for a planned follow-on feature that assigns fine-grained, per-project permission sets to individual users.
- **Related crates (OSS):** `temps-core` (new `ProjectAccessChecker` trait), `temps-auth` (new `project_access_guard!` macro), all ~18 OSS crates with project-scoped handlers
- **Security review required:** Yes. Per project CLAUDE.md, security-sensitive changes require `security-auditor` sign-off before merge. This ADR governs a missing authorization enforcement boundary in a team-based access-control feature — an IDOR class of gap where any authenticated user with a sufficient instance-wide role can read or modify any project on the instance regardless of team membership.

---

## Context

### The existing team-access feature and the gap

An optional plugin ships a complete data model for team-based project access:

- a `teams` table — named groups of users
- a `team_members` table — user-to-team assignment, with a `role` column (`owner|admin|deployer|viewer`)
- a `project_team_access` table — which teams have access to which projects

The CRUD for this data model is fully implemented and the management UI exists. The onboarding story for this feature is: "create a team, add members, grant the team access to the projects they should see."

That story is currently false. No code outside the plugin's own CRUD service has ever queried `project_team_access` for authorization purposes. The only prior consumer was a now-removed investigation-tooling component. Zero non-CRUD callers remain.

### Why the existing guards do not solve this

Two project-scoping guards already exist in `temps-auth`:

**`permission_guard!(auth, SomePermission)`** (`permission_guard.rs`): Checks that `auth.has_permission(&Permission::SomePermission)`. The permission set is instance-wide — bound to the user's global role (Owner, Admin, Deployer, Viewer, etc.). Granting a user `ProjectsRead` at the instance level means they can read EVERY project. There is no per-project dimension in this check.

**`project_scope_guard!(auth, project_id)`** (`permission_guard.rs:38–80`): Purpose-stated in its own doc comment: "For user/API-key/session/CLI auth this is a no-op (returns Ok)." The underlying method `AuthContext::is_scoped_to_project` (`context.rs:323–331`) returns `true` unconditionally when `self.project_id()` returns `None`, which is the case for every non-deployment-token caller. The guard exists exclusively to prevent a `DeploymentToken` from reaching a different tenant's project (cross-project IDOR for machine credentials). The presence of `project_scope_guard!` in a handler is NOT evidence that project-scoped team access is enforced for human sessions in that handler.

### Measured surface area

A codebase scan across `temps/crates/` (excluding generated entities and migrations) finds:

- **~94 OSS handler functions** that take a `project_id` as a path parameter (`Path(project_id): Path<i32>`), spread across **~18 crates**: `temps-deployments` (9), `temps-environments` (7), `temps-analytics-events` (11), `temps-error-tracking` (13), `temps-agents` (11), `temps-projects` (7), `temps-status-page` (6), `temps-revenue` (6), `temps-vulnerability-scanner` (4), `temps-otel` (4), `temps-analytics-funnels` (4), `temps-ai-chat` (3), `temps-webhooks` (2), `temps-providers` (2), `temps-monitoring` (2), `temps-observability` (1), `temps-log-aggregator` (1), `temps-auth` (1).
- **~117 existing `project_scope_guard!` call sites** across 13 crates — covering deployment-token IDOR, unrelated to the team-access gap.

This means the fix is "add a check to ~94 handlers across ~18 crates," not a trivial wrapping of a handful of endpoints. The design must minimize per-handler boilerplate.

### Why this blocks a planned fine-grained permissions feature

A "custom role" restricts "exactly these permissions on exactly this project." The "on exactly this project" half is not enforced anywhere today. Building that feature without first shipping this fix would produce a feature that visibly does nothing.

### No env-var configuration

Per `temps/CLAUDE.md` ("NEVER: Add new runtime configuration as environment variables"): all configuration is per-record in the database, modifiable via the admin API, with full audit logging. No `TEMPS_*` toggle controls the behavior described in this ADR.

---

## Decision

### 1. New OSS extension point: `temps_core::ProjectAccessChecker`

A new file `temps-core/src/project_access.rs` and a `pub use` re-export from `temps-core/src/lib.rs` (matching the convention already established for `DeploymentGate` in `temps-core/src/deployment.rs`):

```rust
// temps-core/src/project_access.rs  (new file)

use async_trait::async_trait;

/// OSS extension point for team-based project access enforcement.
///
/// OSS itself never implements this trait and has no concept of what
/// governs project access — it just asks "may this user access this
/// project?" A plugin (e.g. one implementing team-based project access)
/// registers an implementation via `context.register_service(checker)`.
/// Callers retrieve it with `context.get_service::<dyn ProjectAccessChecker>()`
/// — never `require_service` — so the OSS binary is a strict no-op when
/// nothing is registered, identical in spirit to [`crate::DeploymentGate`].
///
/// The check is fail-closed on infrastructure errors: if the implementation
/// cannot reach its database it must return `Err`, never silently allow.
/// (This is distinct from the "fail-open when unconfigured" behavior, which
/// is a semantic property of the checker's own business logic when the
/// plugin is present but no access grants have been configured yet —
/// see "Fail-open/fail-closed semantics" below.)
#[async_trait]
pub trait ProjectAccessChecker: Send + Sync {
    /// Returns `Ok(true)` if `user_id` may access `project_id`, `Ok(false)`
    /// if they may not, or `Err` if the check could not be completed
    /// (infrastructure failure — the caller treats this as a denial).
    async fn user_can_access_project(
        &self,
        user_id: i32,
        project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
}
```

This is the ONE extension point for this class of feature. A future per-project ACL concept (e.g. per-environment access grants) reuses or extends this trait rather than requiring a new OSS hook.

### 2. New OSS macro: `project_access_guard!` in `temps-auth`

Add to `temps-auth/src/permission_guard.rs`:

```rust
/// Guard that enforces team-based project access for human sessions.
///
/// This is DISTINCT from and ADDITIONAL TO `project_scope_guard!`, which
/// handles deployment-token cross-project IDOR and is a no-op for human
/// sessions. This macro enforces `project_team_access` for those same
/// human sessions, but only when a `ProjectAccessChecker` has been
/// registered (i.e. only when the team-access plugin is present).
///
/// Call order in every project-scoped handler:
///
///   permission_guard!(auth, SomePermission);          // 1. instance-wide role
///   project_scope_guard!(auth, project_id);           // 2. deployment-token IDOR
///   project_access_guard!(auth, project_id, checker);  // 3. team-based access
///
/// `checker` is `&Option<Arc<dyn temps_core::ProjectAccessChecker>>`,
/// resolved from the service registry during the plugin's `configure_routes`
/// phase and stored in each handler plugin's AppState. When `checker` is
/// `None` (no team-access plugin present) this macro is a synchronous
/// no-op with zero overhead.
///
/// Deployment tokens bypass this guard entirely — they are already confined
/// by `project_scope_guard!` above and have no user identity with which to
/// look up team membership.
#[macro_export]
macro_rules! project_access_guard {
    ($auth:expr, $project_id:expr, $checker:expr) => {
        // Deployment tokens: already handled by project_scope_guard! — skip.
        if !$auth.is_deployment_token() {
            if let Some(ref checker) = $checker {
                if let Some(user_id) = $auth.user_id_opt() {
                    match checker.user_can_access_project(user_id, $project_id).await {
                        Ok(true) => {}
                        Ok(false) => {
                            return Err(temps_core::error_builder::ErrorBuilder::new(
                                ::axum::http::StatusCode::FORBIDDEN,
                            )
                            .type_("https://temps.sh/probs/project-access-denied")
                            .title("Project Access Denied")
                            .detail(
                                "Your team membership does not include access to this project",
                            )
                            .build());
                        }
                        Err(e) => {
                            ::tracing::error!(
                                project_id = $project_id,
                                user_id = user_id,
                                error = %e,
                                "ProjectAccessChecker infrastructure failure — denying access"
                            );
                            return Err(temps_core::error_builder::ErrorBuilder::new(
                                ::axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            )
                            .type_("https://temps.sh/probs/project-access-check-failed")
                            .title("Project Access Check Failed")
                            .detail(
                                "Could not verify project access; please try again",
                            )
                            .build());
                        }
                    }
                }
                // user_id_opt() returning None means the caller is a deployment
                // token (already handled above) or an unauthenticated request
                // (impossible at this point — RequireAuth runs first). No case
                // requires an else branch here.
            }
            // checker is None → no plugin registered → no-op.
        }
    };
}
```

### 3. Per-plugin `AppState` addition

Every handler plugin that serves project-scoped routes adds one field to its `AppState`, resolved during `configure_routes` exactly as `temps-deployments` resolves `deployment_gate` today:

```rust
// In each plugin's configure_routes():
let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

// In each plugin's AppState struct:
pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
```

`configure_routes` runs after `initialize_plugin_services` for all plugins, so by the time each plugin constructs its `AppState`, a checker registered by the team-access plugin is guaranteed to be present in the registry. This matches the comment in `temps-deployments/src/handlers/types.rs:44–49` and the identical reasoning for `deployment_gate`.

Affected crates (18): `temps-deployments`, `temps-environments`, `temps-analytics-events`, `temps-error-tracking`, `temps-agents`, `temps-projects`, `temps-status-page`, `temps-revenue`, `temps-vulnerability-scanner`, `temps-otel`, `temps-analytics-funnels`, `temps-ai-chat`, `temps-webhooks`, `temps-providers`, `temps-monitoring`, `temps-observability`, `temps-log-aggregator`, `temps-auth`.

### 4. The team-access plugin registers the `ProjectAccessChecker` implementation

Inside the plugin's `register_services`, after its existing team-service registration:

```rust
// (addition to register_services in the team-access plugin)

let checker: Arc<dyn temps_core::ProjectAccessChecker> =
    Arc::new(TeamProjectAccessChecker::new(db.clone()));
context.register_service(checker);
```

`TeamProjectAccessChecker` is a new struct in the plugin's own source tree. It does NOT implement the plugin's existing team-service trait — it is a separate, focused type whose only job is to answer the authorization question.

### 5. `TeamProjectAccessChecker` semantics: fail-open-when-unconfigured

The authorization logic in `TeamProjectAccessChecker::user_can_access_project`:

```
1. If the user holds Admin or PlatformAdmin role → allow.
   (Instance owners are always unrestricted. The checker receives user_id,
   not the role, so this requires a lightweight DB lookup or an extra field
   on the call — see §Implementation Notes below.)

   NOTE: The checker only receives user_id and project_id. Determining
   admin status requires either (a) passing AuthContext to the trait method,
   or (b) looking up the user's role from the database. Option (a) introduces
   a dependency on temps-auth types into temps-core. Option (b) adds one
   extra query. Preferred: the guard macro short-circuits before calling the
   checker when auth.is_admin() is true (see the macro sketch — implementer
   may add this early exit to the macro body to avoid the round-trip).

2. If there are NO team-membership rows for this user → allow.
   (User has never been placed in any team — they are not subject to team
   scoping. This is "unrestricted until explicitly restricted.")

3. If there are NO `project_team_access` rows for this project_id → allow.
   (Project has never been restricted to any team. Adding the FIRST team
   access grant to a project is the action that makes it team-gated.)

4. Otherwise: the user must be a member of at least one team that has a
   `project_team_access` row for this project_id.
   - If yes → allow.
   - If no → deny (return Ok(false)).
```

**Why "project-level access-grant semantics" instead of "user-in-any-team → fully restricted":**

Rule 3 ensures that an operator who enables team-based access and creates teams cannot accidentally lock users out of projects they never restricted. A project must be explicitly granted to at least one team before team scoping applies to it. This prevents two classes of accidental lockout:

- An instance with the team-access plugin present but no access grants configured yet: all projects remain accessible to all appropriately-roled users, identical to today.
- An operator who has set up teams for some projects but not others: only the explicitly granted projects become team-gated; untouched projects remain open.

**Why "user-in-no-teams → unrestricted" (rule 2) instead of "user-in-no-teams → blocked from all team-gated projects":**

If a project is team-gated (has at least one access grant) and a user has no team memberships, blocking them is correct — they have not been granted access to a restricted project. Rule 2 is NOT that they get to bypass all team gates; rule 2 only covers projects that have ZERO access grants (rule 3 already handles the team-gated case correctly). The two rules compose correctly: a user in no teams can access unrestricted projects (rule 3 → allow) but cannot access team-gated projects (falls to rule 4 → deny, because they are in no team with access).

**Revised rule 2 (more precise):**

Rule 2 should read: "If this user has no team memberships AND the project has no team access grants → allow (rule 3 already covers this case; rule 2 can be simplified away — the two-step check is sufficient)."

The cleanest implementation:
1. Query: does this project have any `project_team_access` rows? If no → allow.
2. Query: does this user belong to any team that appears in those rows? If yes → allow. Else → deny.

This collapses rules 2 and 3 into a single two-step check that is correct for all cases, including the case where the project is unrestricted (short-circuits at step 1) or the user is in no teams (step 2 returns an empty intersection → deny, which is correct because the project IS team-gated).

**Special case: admin bypass.** The macro adds an early exit before calling the checker:

```rust
// Add to the macro body, before calling checker.user_can_access_project:
if $auth.is_admin() || $auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
    // Instance administrators are never restricted by team membership.
} else {
    // ... proceed with checker call
}
```

This keeps the admin bypass in the OSS macro rather than requiring the plugin's implementation to duplicate role-awareness logic.

### 6. Caching

The check requires up to two sequential DB queries per project-scoped request (project has grants? → user is in a granted team?). For the expected load of a PaaS control plane (not the hot-path proxy), per-request queries are acceptable during the initial rollout, but a short-lived in-process cache is warranted to keep the additional latency invisible under UI-interactive usage.

**Cache shape:** `moka` (already used elsewhere in this codebase at version `0.12`) with:
- Key: `(user_id: i32, project_id: i32)`
- Value: `bool` (is_allowed)
- TTL: 60 seconds (team membership and project access changes are infrequent admin actions; 60s delay between a membership revocation and its enforcement is acceptable and should be documented in operator-facing documentation)
- Max capacity: 10,000 entries (a control plane serving 500 users × 20 projects = 10,000 unique pairs, which bounds memory to a few hundred KB)
- Cache invalidation: explicit invalidation on writes to the team-membership and project-access tables via the plugin's own service methods. Implementer must ensure `grant_project_access`, `revoke_project_access`, `add_member`, and `remove_member` call `checker.invalidate(user_id, project_id)` or `checker.invalidate_project(project_id)` after the DB write succeeds.

`TeamProjectAccessChecker` owns the `moka::future::Cache` as a field. No shared cache between checker instances (there is exactly one checker instance, registered as a singleton in the service registry).

The fail-closed contract (§1 trait comment) applies only to infrastructure errors (DB unreachable). A cache hit returning `false` is a legitimate deny, not an infrastructure error.

---

## Security Considerations

### IDOR gap being closed

The gap being closed is an IDOR class of vulnerability: any authenticated user whose instance-wide role grants `ProjectsRead`/`DeploymentsWrite`/etc. can currently read or modify any project on the instance, regardless of team membership. On a multi-tenant self-hosted instance where different teams are meant to be isolated, this means a user in "Team Engineering" can access "Team Finance"'s deployments, environment variables, error logs, and analytics simply by knowing or guessing the project IDs. Project IDs are sequential integers starting at 1.

### Fail-closed on infrastructure error

When `TeamProjectAccessChecker` cannot reach its database to evaluate the check, it must return `Err`. The macro converts `Err` to HTTP 500 and logs the error. This is intentional: a broken access control check must not silently allow access. The 500 response surface is acceptable — it is the same response a handler would produce if its own service call fails.

### Error response: 403, not 404

A user who is denied access to a project because of team membership receives HTTP 403. The resource's existence is not hidden from them by HTTP 404, because:

- Instance-wide `ProjectsRead` is a prerequisite (checked by `permission_guard!` first) — they already know projects exist from the project list endpoint, which is instance-wide and not filtered by team at this layer.
- 404 hiding would be deceptive given that the list endpoint already confirmed existence.
- 403 is the semantically correct response for "you are authenticated, you have the instance-wide role, but you are not permitted to access this specific resource."

Future work: if the project list endpoint is also filtered by team membership (a desirable UX improvement, not part of this ADR), then 404 hiding on individual project endpoints becomes more appropriate — that is a follow-up decision.

### API keys and CLI tokens

Both are **in scope** for this ADR:
- `AuthSource::ApiKey`: carries a `user` field (`Some(users::Model)`) — team membership is a user-level restriction and applies regardless of the credential type used.
- `AuthSource::CliToken`: same reasoning.

Excluding API keys from team scoping would create a trivial bypass: a user in a restricted team could mint an API key and use it to access any project. This would make the team-access feature meaningless for any operator who issues API keys to users.

`AuthSource::DeploymentToken`: excluded. Deployment tokens are already bound to a specific project at issuance (enforced by the existing `project_scope_guard!`) and carry no user identity. The `project_access_guard!` macro explicitly skips deployment tokens (`!$auth.is_deployment_token()`).

### No new attack surface in OSS

An OSS binary (no `ProjectAccessChecker` registered) has identical behavior to today: the `project_access_guard!` macro is a synchronous no-op that branches on `Option::None` with no DB access, no network call, and no latency.

### Audit logging

Adding the guard to project-scoped handlers does not itself require new audit log entries — the existing audit log for each handler operation already records user, operation, and project ID. The access-denied response (403) is logged at the `warn` level by the macro. Implementations that change team membership and project access grants already emit audit log entries per `temps/CLAUDE.md`'s requirement that all write operations include audit logging.

---

## Alternatives Considered

### Option A: Axum middleware filtering by route pattern

Apply the check as a middleware layer that extracts `project_id` from the path and runs the team-access check before the handler executes. Avoids touching every handler.

Rejected for v1: extracting path parameters inside middleware requires matching against Axum's `MatchedPath` and manual string parsing — not supported cleanly in Axum 0.8's layered architecture. The resulting middleware would be fragile (silently skipping routes where the path parameter name differs from `project_id`, or where the project is at a non-standard path position). The per-handler macro approach is explicit and verifiable by a static check (see §Test Requirements).

### Option B: Filter the project-list endpoint only

Instead of per-handler enforcement, filter `GET /projects` to return only team-accessible projects, and trust that users won't guess IDs they've never seen.

Rejected: security through obscurity. Sequential integer IDs (starting at 1) are trivially enumerable. Access control must enforce at every resource endpoint, not rely on list filtering as the sole gate.

### Option C: Extend `AuthContext.custom_permissions` per-project at session time

At login, load all team memberships and pre-compute a "this user can access project IDs {1, 3, 7}" set baked into the session token or session record.

Rejected: sessions are long-lived (hours to days); baking project access into a session means revocation is not effective until the session expires. Team membership changes should take effect within one cache TTL (60s per this ADR), not at next login. Additionally, sessions are instance-wide and the project access set is large and variable — baking it in would bloat session storage and add a complex re-validation cycle on every team change.

### Option D: Database-level row security (PostgreSQL RLS)

Use PostgreSQL row-level security policies on project tables, enforcing team membership at the DB layer.

Rejected: the Temps data model uses a shared schema with Sea-ORM and does not set per-connection database roles. Adding RLS would require either a connection-per-user model (incompatible with the connection pool architecture) or dynamic `SET LOCAL` role switching per query (brittle and untested in this codebase). The application-layer check is simpler and consistent with every other access control decision in this codebase.

---

## Consequences

### Positive
- Closes a real, currently-shipped correctness gap in an already-shipped team-access feature. Operators who configured team-based project access will get the isolation they expect.
- No behavior change for OSS-only binaries — the guard is a strict no-op when no `ProjectAccessChecker` is registered.
- No behavior change for instances with the team-access plugin present but no access grants configured — "project-level access-grant semantics" ensures all projects remain accessible until at least one team grant is added.
- Directly unblocks a planned fine-grained permissions feature, whose entire value proposition requires this enforcement layer to be active first.
- Uses the established `DeploymentGate`-style optional extension point pattern — no new architectural precedent, no new class of coupling.

### Negative
- ~94 OSS handler functions across ~18 crates each require three changes: one field added to the plugin's `AppState`, one line in `configure_routes` to resolve the checker, and one `project_access_guard!` call in the handler body. This is a large but mechanical change.
- Adding `moka` as a direct dependency of the team-access plugin (currently, moka is only used in a couple of other crates in this codebase). The version `0.12` is already in the workspace; this is a new dependency declaration, not a new package fetch.
- A 60-second delay between an admin revoking team access and that revocation taking effect for in-flight sessions. This is the same latency class as other short-TTL caches already used in this codebase and is acceptable for the control-plane use case.

### Risks
- **Coverage gaps**: if any project-scoped handler is missed (no `project_access_guard!` added), that endpoint remains an unprotected bypass. Mitigated by the test requirement in §Test Requirements below — a compile-time or CI-time enumeration test that fails if any registered route accepting `/{project_id}/` is missing the guard.
- **Admin bypass correctness**: the `is_admin()` early exit in the macro must match the instance-wide role semantics exactly. If a new privileged role is added to the `Role` enum in the future, the bypass condition must be updated. This should be noted in the `Role` enum's documentation as a cross-cutting concern.
- **Incorrect "project unrestricted" detection on high-write instances**: if a team access grant is added and the cache has not yet expired for that project, users who should now be denied may still be allowed for up to 60 seconds. Mitigated by explicit cache invalidation on writes to `project_team_access`.

---

## Implementation Notes

### Migration needed

No OSS database migration. The `project_team_access` table already exists (created by the team-access plugin's own migration). No schema changes are required by this ADR.

### Breaking changes

**For instances with the team-access plugin present and `project_team_access` rows configured:** this is a behavior change — those rows will now actually gate access. This is a bug fix from the operator's perspective (they configured teams expecting enforcement), but operators who are relying on the current behavior (no enforcement) as a workaround for some other issue will need to verify their team configuration before upgrading. This must be called out in the changelog and release notes as a security fix.

**For OSS-only instances:** no behavior change.

**For instances without the team-access plugin present, or with it present but no access grants configured:** no behavior change.

### Changelog/release note requirement

The ADR records that this is a security fix for a gap in an already-shipped feature. The release notes for the version that ships this change MUST include a section titled "Security: project-scoped RBAC enforcement now active" describing: (a) what was previously not enforced, (b) what is now enforced, (c) the "project-level access-grant" semantics (projects with no team grants remain unrestricted), and (d) a recommendation for operators to review their team-access configuration before upgrading.

### Affected crates (all changes)

OSS:
- `temps-core`: new file `src/project_access.rs`, `pub use` in `lib.rs`
- `temps-auth`: new `project_access_guard!` macro in `permission_guard.rs`, re-export in `lib.rs`
- 18 crates listed in §3: `AppState` field addition + `configure_routes` resolver + `project_access_guard!` call in each project-scoped handler

Plugin side (separate repository, tracked independently):
- the team-access plugin's crate: new `TeamProjectAccessChecker` module, `moka` dependency, call `context.register_service(checker)` in `register_services`, explicit cache invalidation in `grant_project_access`, `revoke_project_access`, `add_member`, `remove_member`

---

## Test Requirements

1. **OSS no-op regression**: call a project-scoped handler on an OSS binary (no `ProjectAccessChecker` registered) with a valid user and any project ID — must return 200, not 403. Verifies the macro does not break OSS behavior.

2. **Unrestricted project**: with the team-access plugin present, a project with zero `project_team_access` rows — any user with the required instance-wide permission can access it, regardless of team membership.

3. **User in no teams, team-gated project**: a project with at least one `project_team_access` row, and a user who is in no teams — returns 403.

4. **User in Team A, project granted to Team A**: user can access the project.

5. **User in Team A, project NOT granted to Team A (but granted to Team B)**: returns 403.

6. **Admin bypass**: a user with `Role::Admin` or `Role::PlatformAdmin` can access any project regardless of team membership, even if the project is team-gated and the admin is not in any team.

7. **API key bearer**: an API key belonging to a user who is denied access to a project (per test 5) returns 403 when used to call a project-scoped endpoint. Verifies API keys are in scope.

8. **Deployment token unaffected**: a valid deployment token bound to project 7 continues to access project 7's endpoints and is denied project 8's endpoints — behavior unchanged from before this ADR. The `project_access_guard!` macro must not alter deployment-token handling.

9. **Infrastructure failure → 500, not 200**: if `TeamProjectAccessChecker` returns `Err` (simulated DB failure), the macro returns HTTP 500, not 200. Verifies fail-closed on infrastructure error.

10. **Cache invalidation on revoke**: revoke a team's access to a project, then immediately call a project-scoped endpoint as a member of that team — must return 403 (cache is explicitly invalidated on write, not just TTL-expired). This test must NOT sleep for 60 seconds; it verifies explicit invalidation.

11. **Coverage enumeration test**: a test in `temps-auth` (or a CI linting step) that enumerates every OSS route registered under a path containing `{project_id}` and asserts that the handler function source contains a `project_access_guard!` call. This test must fail the build if any project-scoped handler is added without the guard, preventing future omissions.

---

## Phased Rollout

**This ADR is Phase 0** (prerequisite to a planned fine-grained permissions feature).

The implementation should ship as a single PR to `temps` (OSS) that:
1. Adds `ProjectAccessChecker` to `temps-core`
2. Adds `project_access_guard!` to `temps-auth`
3. Threads the field and call through all 18 affected crates

...followed by a separate PR to the team-access plugin's own repository that:
1. Implements `TeamProjectAccessChecker`
2. Registers it in the plugin's `register_services`
3. Adds cache invalidation to write methods

The two changes should land together (or the plugin-side change immediately after the OSS PR merges) so the enforcement is never in a half-wired state on a deployed instance.

The planned fine-grained permissions feature must not ship before both changes from this ADR are live.
