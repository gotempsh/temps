<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-101: Internal access grants let another project call a project's `*.temps.local` routes

**Status:** Proposed — 2026-11-09

Related: ADR-098 (the internal proxy this changes), ADR-052 §4 and ADR-055 phase 7 (network policy rules, nftables),
ADR-011 and ADR-024 (the internal zone).

## Context

The worker's internal proxy (ADR-098) serves `<environment>.<project>.temps.local`
to containers on its node. It identifies the caller from the TCP source
address: the local Docker daemon maps the address to a container id, and the
route snapshot maps that container id to the projects it serves
(`RouteStore::container_id_has_project`). A request is served only when the
caller's project is the route's project; the cached identity is then
re-checked against Docker (`source_container_is_current`, behind a per-project
token bucket). Everything else is answered `403 internal route is not
accessible from this workload`.

So every environment of a project can call every other environment of the same
project, and no other project can call it, with no way to change either. Teams
that split one product into several projects (an API, a worker, a billing
service) cannot let them talk over the internal zone. Their only option is
the public address, which costs a round trip through the public ingress and
exposes the call to the internet.

`temps-network-policy` already stores per-workload allow and deny rules, but the
internal proxy does not consult them.

## Decision

### D1. Grants live in their own table, served by the network-policy crate

A grant is a row of a new table, `internal_access_grants`:

| column | |
|---|---|
| `target_project_id` | `NOT NULL`, FK `projects(id) ON DELETE CASCADE`: the project being called, which owns the grant |
| `target_environment_id` | FK `environments(id) ON DELETE CASCADE`; `NULL` = every environment of the target, present and future |
| `caller_project_id` | `NOT NULL`, FK `projects(id) ON DELETE CASCADE` |
| `caller_environment_id` | FK `environments(id) ON DELETE CASCADE`; `NULL` = every environment of the caller, present and future |
| `description`, `created_by`, `created_at` | |

`CHECK (caller_project_id <> target_project_id)`, and a unique index over the
four scope columns (with `COALESCE(.., 0)` for the nullable ones), so the same
grant cannot be stored twice, even by two concurrent requests.

The service and its HTTP handlers are a module of `temps-network-policy`
(`internal_access`), so there is one crate, one plugin and one API tag family for
"what may talk to what". The storage is not `network_policy_rules`, for these
reasons:

1. **There is no project subject, and adding one would break every node's
   enforcement.** A rule's subject is a workload (`deployment` = one
   environment, `sandbox`, `service`, `function`, `job`). A project-wide grant,
   one that covers environments created later, has no workload kind.
   `NetworkPolicyService::node_snapshot` parses every row with
   `ParsedRule::from_row` and fails the whole snapshot on the first row it cannot
   parse. One `subject_kind = 'project'` row would therefore stop policy
   compilation on every node, and so would a control plane rolled back to a
   version that does not know the kind.
2. **Rules are L3/L4 and admin-only; grants are L7 and owned by the project.**
   Rules compile to nftables and are written only by instance administrators
   (`SystemAdmin`), because a rule decides what a workload can reach on any port.
   A grant decides only whether the internal HTTP proxy answers. It is created by
   whoever manages the target project, and it has no ports, protocol or deny
   action. If rules were reused, an operator's L3 ingress allow would
   silently become an L7 grant as well, and the reverse would also happen.
3. **Deletion would not cascade.** A rule's subject and peer are polymorphic
   (`subject_id`, `peer_ref` text) with no foreign key, so removing a
   project's grants on both sides would take triggers on every owning table.
   Here the foreign keys do it.
4. **Grants are allow-only by construction.** Nothing in this table can deny
   same-project traffic, so the default can only widen, never narrow, and there
   is no precedence to define against rules' deny action.

Rules keep governing packets, as before, and are not consulted by the internal
proxy. A grant does not open any port at L3. Under a `restricted` ingress
profile with nftables enforcement, the operator still adds the L3 rule.

### D2. Semantics

The proxy answers a request from container *c* for a route of environment *E*
of project *P* when one of these holds:

1. *c* serves an environment of *P* (unchanged: any environment of the same
   project).
2. *c* serves environment *E′* of project *P′ ≠ P*, and a grant exists with
   `target_project_id = P`, `target_environment_id ∈ {NULL, E}`,
   `caller_project_id = P′` and `caller_environment_id ∈ {NULL, E′}`.

So all four combinations are expressible: project → project, project →
environment, environment → project, environment → environment. Granting is
directional: a grant on *P* for *P′* does not let *P* call *P′*.

With no grant rows, rule 2 never holds and behaviour is exactly as before this
ADR. Self-grants are refused (`CHECK`, and a 400 with a reason).

### D3. Who may change grants

- **List** (`GET /projects/{project_id}/internal-access`): `projects:read`,
  narrowed to the project (`project_permission_guard!`), plus
  `project_scope_guard!` for deployment tokens.
- **Create and delete**: `projects:write` on the **target** project
  (`project_permission_guard!`), and deployment tokens are refused
  (`deny_deployment_token!`). A token minted for a workload must not widen
  what may reach it. On create the caller project must also be accessible to
  the user (`project_access_guard!`). Otherwise a member of one project could
  probe which project ids exist and learn their names from the response.
  Instance administrators pass both checks, as everywhere else.
- Every create and delete writes an audit entry
  (`INTERNAL_ACCESS_GRANT_CREATED`, `INTERNAL_ACCESS_GRANT_DELETED`) carrying
  both scopes. Grants removed by a project or environment deletion (D4) are
  covered by that deletion's own audit entry.
- At most **100 grants per target project**, so a snapshot's per-route caller
  list stays small (D5).

### D4. Deleting a project or environment removes its grants on both sides

- A project row deleted: the foreign keys cascade, as target and as caller.
- A project fenced for deletion (`is_deleted` set) and an environment
  soft-deleted (`deleted_at` set, which no foreign key sees): `AFTER UPDATE`
  row triggers delete every grant naming it, as target or as caller, in the
  same transaction. The triggers run whatever code path writes the row.
- A soft-deleted environment that is later restored does not get its grants
  back. Restoring access is a new, audited grant.
- Create refuses a deleted or fenced project or environment (404), and an
  environment that does not belong to the project it is named with (400).

### D5. Propagation: a per-route caller list in the route snapshot

`RouteEntryDto` (`temps-routes`) and the agent's `RouteEntry` gain:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub allowed_callers: Vec<temps_core::internal_access::InternalAccessCaller>,
// InternalAccessCaller { project_id: i32, environment_id: Option<i32> }
```

`build_snapshot` loads every grant whose projects and environments are all live
in one query, and attaches to each internal route (`routes`, never the public,
private, gated or Shield lists) the deduplicated, sorted callers whose grant
covers the route's project and environment. It costs O(routes + grants) per
snapshot, and the list is bounded by D3's limit.

Compatibility, by construction rather than by a capability flag:

| plane | agent | result |
|---|---|---|
| new | old | The old agent's `SnapshotRoute` has no field for the list and serde ignores it: same-project only. Fails closed. |
| old | new | No field, so `#[serde(default)]` gives an empty list: same-project only. |
| new | new | Grants enforced. |
| any | rolled back from new | `routes.json` may carry the field; the older binary ignores it on load. |

No capability header is needed, because ignoring the field denies; it never
widens access. That is the opposite of the gate and Shield cases (ADR 065 D7,
ADR 066 D10), where an older agent ignoring a field would have served a route
open.

**Revocation latency.** An insert, update or delete on `internal_access_grants`
(including the cascades and triggers of D4) fires
`pg_notify('route_table_changes', 'internal_access')` from a row-level
trigger. The route table reloads and claims a new generation, and every
agent's waiting long poll returns at once with the new snapshot. In practice
that takes about one database round trip plus one snapshot build, well under
a few seconds. If the notification is lost (a silently dead `LISTEN`
connection), the control plane's idle reconcile reloads anyway, so the bound
is `IDLE_RECONCILE_INTERVAL` (60 s) plus one snapshot. That bound holds for an
agent that is connected. An agent that cannot reach the control plane keeps
the grants of its last applied snapshot, as it keeps every other route, until
it reconnects. Grants have no separate lease.

### D6. The agent's check

`RouteStore` keeps, per container id, the set of `(project_id, environment_id)`
pairs it serves, taken from the same route backends that already gave
`container_id_has_project`. The decision is a pure function
(`internal_access::decide`) over the route and the caller's identities:

1. The route has no `project_id`: **deny**, as before.
2. No identity is known for the caller's container: **deny**.
3. An identity's project is the route's project: **allow (same project)**.
4. An identity matches an entry of `allowed_callers` (same project, and the
   entry's environment is unset or equal): **allow (granted)**.
5. Otherwise: **deny**.

Both allows still require `source_container_is_current` (Docker says the
container is running and owns the source address). The inspection quota is
charged to the **caller's** project, so a granted caller cannot use up the
target's budget. Allows through a grant are logged at `debug`. Denials keep
the `warn`, with the route's project, the host and whether the route carries
any grants.

The 403 body says what to do, without naming any other project:

```
Forbidden: internal route is not accessible from this workload. Calls from
another project need an internal access grant on the project that owns this
address: open that project's Settings → Protection → Internal access and allow
the calling project (or one of its environments).
```

### D7. Surfaces

- API (`Internal Access` tag): `GET` and `POST /projects/{project_id}/internal-access`,
  `DELETE /projects/{project_id}/internal-access/{grant_id}`. The grant id is
  always looked up together with the project id, so a grant of another project
  is a 404. List responses name both sides, with names and slugs, sorted
  `created_at DESC` and paginated (default 20, max 100).
- Console: an **Internal access** card on the project's Protection page (whole
  project) and on each environment's settings (grants that target that
  environment, plus project-wide ones shown as inherited). The card says "Allow
  project X (all environments | environment Y) to call this
  project/environment". With no grant it shows what a grant would do and the
  default ("Only this project's own environments can call it"), instead of an
  empty list.
- Diagnostics: the DNS *Test resolution* tool, asked as a project, reports for
  a `*.temps.local` name owned by another project whether that project's grants
  admit the caller. When they do not, it links to the owner's Internal access
  card.

## Consequences

- One more column on the wire and one more query per route snapshot. The
  internal proxy's hot path gains one hash lookup and a short scan of a bounded
  list, and only for requests that already fail the same-project check.
- Grants are an L7 control only. Whoever needs L3 isolation between projects
  keeps using network policy rules, and the two are documented as independent.
- A partitioned agent cannot learn of a revocation until it reconnects (D5).
  Adding a lease to grants would close that gap, but cross-project calls
  would then fail during every control-plane outage. Not done here.

## Alternatives considered

- **`network_policy_rules` with new `project` subject and peer kinds.** Rejected
  for D1's reasons. The fleet-wide snapshot failure on an unknown kind alone
  rules it out without a second migration of every reader.
- **Environment-only grants expressed as existing `deployment` rules.** These
  could express environment → environment, but not project scope, and they
  would share the admin-only write path and the L3 meaning.
- **A capability header for grant-aware agents.** Not needed: ignoring the field
  denies (D5).
