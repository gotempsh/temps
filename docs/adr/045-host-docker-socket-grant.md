# ADR 045: Host Docker socket grant for operator-owned infrastructure services

**Status:** Proposed
**Date:** 2026-09-21
**Related:** ADR 040 (build/node decoupling), ADR 042 (multi-node join)

## Context

Temps never hands a deployed container the host's Docker engine. The compose
executor rejects `privileged`, `use_api_socket`, `cap_add`, `devices` and any
absolute host-path bind; the image deployer builds a `HostConfig` with
`cap_drop: ALL`, `no-new-privileges` and exactly one bind (the secrets tmpfs).
That is the right default: a container holding `/var/run/docker.sock` is root
on the host and can read every other tenant's volumes.

There is one class of workload that legitimately needs the socket and that an
operator wants to run *through* Temps rather than beside it: a per-host agent
whose job is to manage containers on that host on behalf of an external
control plane (a node daemon, a runner, a local registry mirror). Running it as
a Temps project gives it what every other project gets for free -- image
builds, blue/green rollout with health gating and rollback, routing, logs,
metrics -- and none of that is available to a hand-installed systemd unit.
Compose is not an answer here: a compose deploy recreates in place and takes
the service down for the duration.

The question is how to allow that for a handful of operator-owned services
without weakening the default for everything else.

## Decision

A **host-level, operator-set grant**, keyed by project slug, evaluated by the
process that actually creates the container.

### The grant is host policy, not project configuration

`TEMPS_DOCKER_SOCKET_PROJECTS=<slug>[,<slug>...]` is read once at startup by
each process that can run a deployment: `temps serve` for local placement,
and `temps agent` on every worker. Unset means the current behaviour, on every
host, unconditionally.

This is deliberately an environment variable rather than a column, following
`TEMPS_ALLOWED_POSTGRES_DOCKER_IMAGES` and `TEMPS_TRAEFIK_DISCOVERY_ENABLED`:
which projects may hold root on *this machine* is a decision for whoever has a
shell on this machine. It must not be reachable through the API, because any
account or token that could flip it would be one step from the host. It is
also naturally per host -- a worker that never opted in cannot be granted by
the control plane, and vice versa.

### The bind is added where the container is built

`DeployRequest` carries the project slug. The image deployer's single
`HostConfig` build site compares that slug against its own process's parsed
grant set and, on a match, adds exactly one bind:
`/var/run/docker.sock:/var/run/docker.sock`. Nothing else changes:
`cap_drop: ALL`, `no-new-privileges`, the PID limit, the read-only secrets
tmpfs all stay. The container is not privileged; it holds one file descriptor
that happens to be root-equivalent, which is the minimum the workload needs.

Because `DeployRequest` is serialised verbatim to the worker agent and the
agent runs the same deployer code, the decision is made by the executing
process against its *own* environment in both the local and the remote case.
The control plane never tells a worker "mount the socket"; it tells it "this
is project X", and the worker answers from its own policy.

### Placement is gated up front, never discovered mid-deploy

The same variable answers two different questions depending on where it is
set. On the **control plane** it *declares* which projects require the socket:
that declaration, and nothing else, creates the placement gate. On **each
host** it decides whether *that host* provides the socket. A worker advertises
its own set in its heartbeat, and those advertisements only ever **narrow**
which hosts a declared project may run on — they never create a gate.

That asymmetry is the security property. Heartbeat capacity is data supplied
by the node: if an advertisement could create the gate, one compromised or
merely misconfigured worker could name any slug and make itself the only
eligible placement for that project — or, once drained, make it unschedulable
cluster-wide. So the scheduler asks its own process grant first; a slug it
does not declare is placed exactly as it was before this ADR, whatever any
node reports. Slugs advertised outside the declared set are ignored for
scheduling and logged at `warn!` when a node's advertised set changes, naming
the node, since the common cause is an operator who set the variable on the
worker and forgot the control plane.

When the gate does apply, the scheduler keeps only nodes that advertise the
grant for that slug (plus the control plane itself, which by declaring it also
grants it, when this process runs local workloads), and records the exclusion
reason per node the way the architecture filter does. If no host qualifies the
deployment fails before anything is built, with the same actionable
worker-node problem the platform already uses, telling the operator to declare
the project on the control plane and grant it on at least one node. A granted
project is never silently placed on a host that would quietly deploy it
without the socket.

The gate is also *verified* rather than assumed. The scheduling result carries
whether the gate applied, and the deploy step compares it against the
executing host's `DeployResult.docker_socket_mounted`: a replica that was
placed because the project requires the socket, on a host that then reports it
mounted none, fails the deployment with a message naming that host and the
variable to set on it. Without that check the one failure the feature exists
to prevent — a half-applied configuration change leaving an infrastructure
service running with no engine access — would be reported as a healthy
deployment.

The companion control is the address the granted workload is *published* on.
A service that holds the socket is root-equivalent on its host, so its own
API must not be reachable from anywhere that host is. `temps agent` already
binds every published container port to the node's registered
`nodes.private_address` -- the WireGuard tunnel IP in relay mode, or the
operator-managed address in direct mode -- and never to `0.0.0.0`; the value
is set with `--private-address` / `TEMPS_AGENT_PRIVATE_ADDRESS`, validated as
an IP literal with reserved ranges (including `0.0.0.0`) rejected at startup,
and logged on every boot. Granting the socket to a project on a node whose
private address is a public IP puts a host-root-equivalent API on the public
internet; that combination is the one an operator must not ship.

This binds the **published host port** only. It does not, by itself, make a
granted project unreachable: the platform's own reverse proxy still serves
the same container on its auto-managed environment subdomain, and on any
custom domain attached to it, regardless of the private-address bind. The
proxy-level password gate (`security.password_protection`) is therefore the
actual control on public reachability for most granted projects, not the
private address -- and, like every other write that changes what a granted
project exposes, is admin-only (see "Nothing else can plant the payload").

"Keeps only nodes that advertise the grant" above is
`NodeService::granting_node_ids_and_names`, which runs on every placement of
a declared project and selects only `id`, `name` and `capacity` -- not the
full `nodes` row (labels, token, address, timestamps) `list_all()` used to
load for this same check. The exact-slug match still happens in Rust via
the existing `capacity_grants()` helper; a
`capacity->'docker_socket_projects' @> '["<slug>"]'` predicate evaluated by
Postgres, matching `ProjectService::docker_socket_capability`'s read-only
capability query, would remove that Rust-side filter entirely but is not
mockable with the `sea_orm::MockDatabase` this module's placement tests
build on throughout -- left as follow-up work that also converts those
tests to a real database.

### It is visible, and it is audited

The project response carries a capability object -- `granted`, `reason`,
`setup_path`, plus the nodes that advertise the grant -- so the console can
show a "Host Docker access" badge on the project header and an onboarding
state explaining what to set and where when it is not granted. `granted`
follows the rule above: it is false until the control plane declares the
project, and a node advertising a slug nobody declared is reported in the
reason as the misconfiguration it is rather than as a grant. Every deployment
that mounts the socket records an audit event naming the project and the node.
The CLI surfaces the same capability.

Claiming a granted slug is itself an admin-only act. `projects.slug` is
writable by any project writer and is derived from the display name at create
time, so without that rule a non-admin could rename a project onto a granted
slug and have its next deployment run as root on every host that grants it.
Creating a project with -- or renaming one onto -- a slug this host grants is
therefore refused with a 403 for anyone who is not an instance admin, named in
the API error and logged with the principal. Renaming a project *away* from a
granted slug is admin-only for the same reason: it revokes that service's host
Docker access everywhere and frees the slug for whoever creates a project next,
so gating only the claim would leave the same outcome open in two requests
instead of one. Only a slug *change* is gated -- an existing granted project
keeps working through every update that does not move its slug, whoever sends
it.

Being an instance admin answers *who may*; it does not answer *is this really
them, right now*. Moving a granted slug in either direction is therefore also
a sensitive action in the existing step-up sense
(`SensitiveAction::ClaimDockerSocketSlug` / `ReleaseDockerSocketSlug`): a
session that has not verified a second factor recently gets a `428` with
`error_code: STEP_UP_REQUIRED` and the action name, and the console re-runs the
save once the operator verifies. The check lives inside `guard_reserved_slug`,
next to the admin check and reached only after it passes, rather than in the
three handlers that can reach it -- a handler-level pre-check is one call site
away from being forgotten, and the fourth caller is the one that matters. It is
consulted *only* for a slug this host actually reserves, so ordinary project
creation and renaming are untouched.

The ordering is deliberate and is asserted in tests: a project writer is
refused with the 403 above *before* step-up is considered. Prompting them to
re-verify would be asking for proof of an identity that still would not be
permitted to do it.

`DefaultSensitiveActionAuthorizer` allows a user with no enrolled MFA factor
through without a challenge -- there is no second factor to re-verify, and
denying would lock the only admin of a fresh instance out of their own
install. That is a deliberate property of the existing policy, not an
oversight in this guard, and it means the step-up raises the bar for operators
who have enrolled MFA without changing anything for those who have not. An
operator who wants the stronger rule enrols MFA, or registers a
`SensitiveActionAuthorizer` that denies unenrolled principals; the guard here
asks the policy and does not second-guess it.

The same policy passes **machine credentials** -- API keys, CLI device tokens
and deployment tokens -- through without a challenge, for the same structural
reason and one more: they have no interactive factor that could ever be
re-verified, and denying them would break scripted and CI usage rather than
add friction to it. `DefaultSensitiveActionAuthorizer` returns `Allow` for
each of them and logs `step_up = "skipped_machine_principal"`. The practical
consequence for this ADR is worth stating plainly, because it is easy to read
"admin-only plus MFA step-up" as stronger than it is: **an admin's API key is
sufficient on its own to move a granted slug.** The step-up narrows the
browser-session path, not the token path. Their blast radius is bounded only
by the key's own role/permissions, which `permission_guard!` checks before the
authorizer runs -- so an admin-scoped API key is, for this purpose, equivalent
to an admin session that has already verified. An operator who wants the
slug-moving path to require a human present should scope machine credentials
below instance-admin, or register a `SensitiveActionAuthorizer` that denies
non-interactive principals for `ClaimDockerSocketSlug` /
`ReleaseDockerSocketSlug`.

The claim-time check above (`guard_reserved_slug`, called from
`create_project_as`/`create_service_project_as`) is authoritative but runs
*after* project creation is already committed to happening. That is a
problem for callers with an irreversible side effect between planning the
slug and calling it: creating a project from a template in fork mode calls
`create_repository_and_push_template` -- an external, irreversible Git
provider API call -- against the already-planned slug before that check
ever ran. A `428 STEP_UP_REQUIRED` at creation time then left an orphaned
external repository with no Temps project behind it, and a retry failed
with "repository already exists" rather than re-prompting for step-up.
`ProjectService::preflight_guard_reserved_slug` runs the identical guard
early, keyed to the same planned slug, for exactly this shape of caller. It
does not replace the creation-time check -- the planned slug can still
change between the two calls via a race -- it only moves the irreversible
side effect to after step-up is satisfied in the common case.

#### What the audit record does and does not prove

The audit event is written on the control plane from the executing host's
**self-report** (`DeployResult.docker_socket_mounted`). It is therefore not
tamper-evident against a compromise of that host: a host that mounts the
socket and reports that it did not would produce no control-plane record. The
deployer's own `warn!` at the bind site carries a stable
`event = "docker_socket_mounted"` field with the project slug and container
name, so host-side log shipping keeps an independent record of the same fact,
and the two can be reconciled.

Similarly, anyone holding a worker's agent token can ask that worker to deploy
a container for any slug that worker grants. The agent token *is* the trust
boundary here: it already authorises arbitrary container creation on that
host, and the grant adds one more thing an attacker who holds it can reach.
Protecting it is unchanged and unchangeable by this ADR.

Compose deployments are unchanged: the deny-list stays as it is. The grant
applies only to the image deployment path.

### Deploying and exec'ing into a granted project are also admin-only

A project writer who cannot claim the slug can still, without this guard,
deploy or exec into a project someone else already granted -- the deploy
gate and the exec gate close that. Both reuse one predicate,
`deploy_requires_instance_admin(grant, slug, caller)`, so they cannot
disagree about which projects are gated or who may act on them.
`DeployCaller` distinguishes a human-attributable actor (`ProjectWriter`,
fail-closed default, or `InstanceAdmin`) from `Platform` -- Temps' own
failover/drain-reschedule code, which carries no attacker-chosen image or
command and is deliberately exempt.

The predicate is enforced at the two chokepoints every deployment-creation
and exec path is structurally required to pass through --
`WorkflowPlanner::create_deployment_jobs` (before any job row is written)
and `DeployImageJobBuilder::build` (as its first statement) for deploys,
`verify_container_exec_access` for both the HTTP exec route and the
WebSocket terminal -- rather than at each handler, which an earlier draft of
this guard did and which a security review then found three separate
handlers could bypass by not calling it. A required constructor argument
cannot be forgotten the way a per-handler call can.

Mounting the socket itself carries the same check from the other side:
`docker_socket_bind_for` requires both the executing host's own grant *and*
a `control_plane_grants_socket` flag the control plane computes from its
**own** `process_grant().declares(slug)` and puts on the `DeployRequest`
(`#[serde(default)]`, fail-closed). A worker is never trusted to assert its
own authorization to mount the socket -- only the control plane's
declaration counts, closing a gap where a stale or leftover worker-side
grant could mount the socket with no cross-check against the control plane
that actually decided placement.

#### Exec authorization survives a rename away from the granted slug

Renaming a project away from a granted slug is itself admin-only (the write
guard above), but a rename does not stop or recreate that project's
already-running containers -- it only stops *future* deployments from being
granted the socket. Before this closure, `verify_container_exec_access`
re-derived socket status from the project's **current** slug on every exec
request, so a rename silently downgraded exec authorization on a
still-socket-mounted container from instance-admin-only to anyone holding
`ContainersExec`, even though that container is exactly as root-equivalent
as it was before the rename.

Closed with a persisted `deployments.docker_socket_mounted` column, set
once from the executing host's own `DeployResult.docker_socket_mounted` at
the same point the existing audit event below is written, and never
cleared -- a deployment that was ever root-equivalent stays sensitive for
as long as any of its containers exist. `guard_exec_against` now refuses
exec when *either* signal fires: the project's current slug is
granted-and-refused, or this specific deployment historically mounted the
socket and the caller isn't authorized for a granted-project deploy. A
per-container flag (touching all seven `deployment_containers` insert
sites) or live Docker-mount inspection (touching the `ContainerDeployer`
trait across local and remote nodes) would also have closed this; the
per-deployment column was chosen as the smallest change that still answers
"was *this* container root-equivalent", since exec targets a container that
belongs to exactly one deployment.

### Nothing else can plant the payload a granted deploy will run as root

Admin-only deploy and exec are necessary but not sufficient: a project
writer doesn't need to deploy anything if they can instead change the input
the *next* deploy -- run by an admin, or by the platform's own
failover/reschedule path -- executes as host root. Every field capable of
supplying that input is therefore behind the same
`DockerSocketWriteRequiresAdmin` guard the deploy and exec gates use,
enumerated exhaustively rather than gated one reported case at a time:

- **Runtime config** -- the persisted `command` and image configuration
  (`ProjectService::update_service_template_runtime`, `upgrade_service_template`).
- **Source definition** -- `main_branch`, `repo_owner`, `repo_name`,
  `directory`, `preset`, `preset_config`, `git_provider_connection_id` and
  `enable_preview_environments` (`update_project_settings_as`), plus the
  sibling handler `update_project` (`PUT /projects/{id}`), which rewrites the
  same fields unconditionally and took no caller at all until it was found to
  bypass the guard entirely. `git_provider_connection_id` was missing from
  the first version of this list -- repointing *which* git connection a
  project trusts is the same attack as repointing `repo_owner`/`repo_name`.
  `enable_preview_environments` was missing from the second -- once on, a
  push to any branch no environment tracks gets a preview environment
  auto-created and deployed as `DeployCaller::Platform`, which neither the
  deploy gate nor the exec gate refuses.
- **Deploy triggers and exposure** -- `automatic_deploy` and `exposed_port`
  (`update_automatic_deploy`, `update_project_deployment_config`), and their
  environment-scoped twins `automatic_deploy`, `protected`, `exposed_port`,
  `password`, `security`, `branch`, `target_nodes` and `target_labels`
  (`update_environment_settings`, `create_environment`,
  `add_environment_domain`, `update_environment_subdomain`, all in
  `temps-environments`). `protected` is the query-level filter that stops a
  push reaching an environment at all; `password`/`security` and the domain
  routes are the only control on whether the platform proxy serves a granted
  project's container publicly on its subdomain -- the private-address bind
  described above only protects the *published host port*, not the proxy
  route, so this is a correction to that section, not just an addition here.
- **Source type and alternate sources** -- `set_source_type`,
  `set_allow_alternate_sources`. Not currently exploitable on their own
  (every image/static/drop deploy path already gates independently via the
  deploy and exec gates), gated anyway as defense in depth.
- **Git settings** -- `git_url` (`update_git_settings`).
- **Environment variables and secrets** -- create, update *and delete* of
  both (`temps-environments`). A delete can't plant a value, but it can
  silently degrade a granted project's infrastructure service (e.g.
  removing the credential it authenticates with) or re-expose a
  lower-precedence, scope-shadowed variable -- kept symmetric with
  create/update rather than gating some of the verbs on a resource. An
  environment variable is delivered into the container verbatim, and a
  secret is materialised as a file under `/run/secrets/<KEY>` -- either is
  enough to get arbitrary code execution in most runtimes once a shell reads
  it (`NODE_OPTIONS`, `PYTHONSTARTUP`, `BASH_ENV`, ...), and `Role::User`
  holds `EnvironmentsCreate`/`EnvironmentsWrite` against any project since
  OSS never registers a `ProjectAccessChecker`.
- **The AI agent** -- the executor's push-and-open-PR step
  (`temps-agents`), a genuinely different subsystem from the three above:
  it commits AI-generated files to a granted project's own repository using
  the project's own stored git connection (so the triggering principal
  needs no git credentials of their own) and emits a `GitPushEvent`, which
  the deployment pipeline treats exactly like a real webhook push
  (`DeployCaller::Platform`). Refused unconditionally, matching
  `SourceDropService`'s existing reasoning (also reachable from the AI
  agent): a run may be triggered by an interactive `Role::User`, an
  automated error-group trigger, or a public webhook trigger, and none of
  those carries an `AuthContext` this guard could check instance-admin
  authority against.

Each of these follows the same shape as the deploy/exec gates: a pure
function taking the grant explicitly (`guard_granted_project_write_against`,
`require_granted_project_write_authority_against`,
`refuse_granted_project_push`) so it is unit-testable without mutating the
process-wide `OnceLock`, plus a thin wrapper that reads `process_grant()` in
production.

**This list has been incomplete four times in a row**, always in the same
way: a hand-maintained `if field.is_some() || ...` predicate missing an
entry, on a sibling of a field that *was* gated. The deploy and exec gates
never had this problem, because they are enforced by a required constructor
argument at a structural chokepoint (`DeployImageJobBuilder::new`,
`WorkflowPlanner::create_deployment_jobs`) rather than a list -- a caller
cannot compile without declaring an authority, so there is no "forgot to
add it" state to reach. The write-guard predicates have no equivalent
property yet: adding a field to `UpdateProjectSettingsParams` or
`UpdateEnvironmentSettingsRequest` compiles whether or not it is added to
the guard. Closing that gap structurally -- an exhaustive classification
every new field must resolve one way or the other before the crate
compiles -- is unshipped, tracked follow-up work, not something this
section's enumeration is a substitute for.

## Consequences

- A granted project is host-root-equivalent on the hosts that grant it, and
  its published ports are only as private as that node's
  `private_address`. Pin that to the overlay/WireGuard address the control
  plane reaches before granting anything.
  This ADR exists so that sentence is written down: the grant is for
  operator-owned infrastructure services, and an operator who grants it to a
  tenant application has removed the isolation boundary on that host.
- Self-hosted instances that never set the variable see no behaviour change
  and no new API-writable surface.
- Operators get zero-downtime rollouts for the one kind of service that used
  to require a hand-maintained unit file, without a general "privileged
  deploy" switch existing anywhere in the product.
- Multi-node works by construction, with the two halves kept apart: the
  control plane's variable *declares* which projects require the socket, each
  host's variable decides whether that host provides it, and heartbeats narrow
  placement without ever creating a requirement. The cost is that an operator
  must set the variable in two places for a worker-hosted grant -- which is
  also the point: nothing a node says can make a project root-equivalent.
