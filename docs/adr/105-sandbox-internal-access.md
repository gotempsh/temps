<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-105: A sandbox can call a project's internal address when that project allows it

**Status:** Proposed — 2026-11-12
**Amends:** [ADR-101](./101-internal-access-grants.md) (a grant's caller can be one sandbox)

Related: ADR-098 (the worker's internal proxy), ADR-101 (internal access
grants), ADR-104 (the control plane serves its own containers' internal
names), and the sandbox egress proxy (per-sandbox sidecar, egress policy modes
`internet` and `restricted`).

## Context

A project's environments are reachable from other containers at
`http://<environment>.<project>.temps.local/`. The internal proxy on the
caller's host routes the call by `Host`. It admits the target's own project,
and another project only through an ADR-101 grant. Sandboxes could not use
these addresses at all:

- A sandbox has no route of its own. It sits on an internal Docker network
  whose only way out is its egress proxy sidecar, and that proxy refuses every
  private, loopback and link-local destination, in every egress mode. An
  internal address resolves to the host's bridge address, so it was always
  refused.
- No grant could name a sandbox. Grants name a project, or one environment of
  a project, and the internal proxy finds a caller's project from its
  container's labels. A sandbox is not a deployment of any project.
- **The second identical `curl` hung.** The egress proxy looked every name up
  with `dns.lookup`, and the sidecar asks the host's resolver through Docker's
  embedded DNS. That resolver hands `.local` names to multicast DNS (RFC 6762)
  and waits out its timeout, about 4 s per lookup on the hosts we measured,
  longer where the system resolver retries. `dns.lookup` runs `getaddrinfo` on
  libuv's thread pool, which has four threads shared by every lookup in the
  process. A few `.local` lookups therefore stalled every later request,
  refusals and public names included. We reproduced it with four concurrent
  `.temps.local` lookups: a public name queued behind them took 5 s.

## Decision

### D1. A grant can name one sandbox

A new table, `internal_access_sandbox_grants`, has these columns:

- `target_project_id`: the owner, as in ADR 101;
- `target_environment_id`: `NULL` means every environment;
- `sandbox_id`, a foreign key to `sandboxes`;
- `description`, `created_by`, `created_at`.

It is a separate table, not a nullable caller column on
`internal_access_grants`. That table's constraints and indexes assume a
project caller, and the old table and the old snapshot field keep their exact
meaning. A rollback to a build without this ADR ignores the new table, and an
agent that predates it parses the new snapshot field as absent. Both fail
closed: no sandbox is admitted.

The rules for a sandbox grant:

- It is created and removed on the target project, with `projects:write` on
  it, never by a deployment token, through
  `/projects/{id}/internal-access/sandboxes`.
- The sandbox must belong to the user creating the grant: `sandboxes.user_id`,
  not destroyed. This is the same ownership rule every sandbox endpoint uses,
  so nobody can name, or probe for, a sandbox they cannot see.
- A target project holds at most 100 grants of both kinds together.
- Creating and deleting a sandbox grant are audit-logged
  (`INTERNAL_ACCESS_SANDBOX_GRANT_CREATED` / `_DELETED`), with the sandbox's
  public id and the name its owner gave it.
- The table is unique on `(target project, target environment, sandbox)`. It
  cascades hard deletes of the project, environment or sandbox.
- Triggers remove a project's grants when it is fenced for deletion, and an
  environment's when it is soft-deleted, as in ADR 101.

**No "every sandbox of user X" subject.** A sandbox runs whatever its owner,
an agent or a prompt-injected tool put in it. "All of a user's sandboxes"
would include ones created later for unrelated work, by automation acting as
that user, that the project owner never saw. A grant names the one sandbox
the owner means. A user with many sandboxes adds one grant per sandbox; the
per-project limit bounds the snapshot either way.

### D2. Identity comes from Docker, never from the request

The internal proxy already identifies callers by source address. It keeps a
snapshot of container IPs from the local Docker daemon. For each admitted
request, it re-inspects the container, rate-limited per key.

For sandboxes, the address that speaks is **the sandbox's egress proxy on the
shared egress network** (`temps-sandbox-egress-v1`). Only that sidecar can
reach the host: the sandbox container itself sits on an internal network with
no gateway.

A container counts as sandbox S's egress proxy only when everything below is
true, as the Docker daemon reports it:

- its name is `temps-sandbox-egress-proxy-v2-S`, and S is a
  `temps-sandbox-*` name;
- it carries the egress-policy label, and the network-owner label names S;
- it is attached to both the egress network and S's own network,
  `temps-sandbox-net-v3-S`;
- the request's source address is its endpoint **on the egress network**.
  Its address on S's network never speaks for S.

Each of these is set by the provider, and none is writable from inside a
sandbox:

- A sandbox runs no Docker client and cannot rename containers.
- Docker names are unique per daemon.
- A Compose deployment cannot set `container_name`, join external or named
  networks, or claim these labels; the Compose security policy refuses them.

Headers are never consulted. The test
`a_sandbox_identity_cannot_be_claimed_with_headers` sends forged
`X-Temps-Sandbox`, `Forwarded` and `X-Forwarded-For` headers, and the proxy
ignores them. Every admitted sandbox request is re-verified with a Docker
inspect: same name, same labels, same networks, and the source IP still the
egress endpoint. So a container that later took over the address is refused.

We considered and rejected:

- **A per-sandbox secret header added by the egress proxy.** It needs secret
  distribution and rotation. The sandbox shares nothing with the sidecar's
  memory, but a leak in logs would let any local container impersonate the
  sandbox.
- **Trusting the `Host`-to-sandbox mapping in the egress proxy.** The
  decision belongs where the grants are, the internal proxy, as for every
  other caller.

### D3. The egress proxy forwards exactly two shapes of name

An internal address is one or two DNS labels, then `temps.local`:
`<environment>.<project>.temps.local`, or the environment's subdomain alias
`<environment subdomain>.temps.local` (D7). In every egress mode,
`restricted` included, a **plain-http request on port 80** for such a name is
sent to this host's internal proxy, with `Host` set to the name. The name is
never looked up, so the decision takes no resolver call: the internal proxy
answers at once, with the grant decision for a route and 404 for a name it
does not route. Everything else stays as it was:

| Request | Answer |
|---|---|
| `http://<env>.<project>.temps.local/...`, `http://<subdomain>.temps.local/...` | forwarded to the internal proxy, which decides with the grants (D4) |
| same name on another port | 403, "port 80 only" |
| `CONNECT` to that name (https, `--proxytunnel`) | 403, "plain http, never https or a CONNECT tunnel" |
| any other `.local` name (three or more labels under `temps.local`, `temps.local` itself, `printer.local`), `localhost`, `*.localhost` | 403 at once, without a lookup |
| a private, loopback, link-local or metadata address, literal or resolved | refused exactly as before |
| a public name whose lookup takes longer than 3 s | 403 at the deadline, "did not resolve within 3 seconds" |

The egress proxy learns where to send internal names from
`TEMPS_INTERNAL_PROXY_ADDRESS`. Temps sets this value in the sidecar's
environment when it creates the sidecar. It is the address the local internal
routing supervisor reports listening:

- the proxy's own port (591) on the app or overlay bridge;
- or the bridge's port 80 in the ADR-098 fallback.

It is never the bridge's port 80 while the redirect serves, because without
the redirect that port may belong to another program.

The supervisor publishes this address in-process. The provider compares it
in its drift check, so a sidecar whose value is stale is replaced the next
time its sandbox's network is ensured (create or start). The Temps binary
reads no new environment variable. This one is configuration of the sidecar
container, which is how the sidecar already receives its policy and name.

### D4. The internal proxy decides from the route snapshot

The route snapshot gains `allowed_sandboxes` per internal route: the sandbox
container names that a sandbox grant on that route's project or environment
admits. A container name is derived from the sandbox's public id
(`sbx_<hex>` runs as `temps-sandbox-<hex>`), never from `sandboxes.name`, which
is a label the owner chooses and may change.

- It is filtered per node. A worker's snapshot lists only the sandboxes
  pinned to that worker; the control plane's lists those with `node_id IS
  NULL`. A sandbox's calls are only ever made on its own host.
- It is computed by the route sync and the control plane's feeder, which
  rebuild on every `route_table_changes` notification, and at least once a
  minute.

The decision on the request path stays lock-free and does no I/O. It uses the
`ArcSwap` route store and a binary search in a sorted, deduplicated list. It
needs one rate-limited Docker inspect per admitted request, the same budget
project callers have.

- A sandbox caller is decided only by sandbox grants. It is never "the same
  project", whatever its labels say.
- Without a grant, the proxy answers 403. The body names the sandbox by the
  public id the console lists (read back off its container name) and says
  where to fix it: open the target project's **Settings > Protection >
  Internal access** and add the sandbox as a caller. The egress proxy relays
  this answer unchanged.

### D5. Where it runs, and when nothing does

| Install | Who answers |
|---|---|
| Single server, cluster DNS on | the ADR-104 internal proxy on the app network's gateway |
| A sandbox on a worker | that worker's internal proxy (ADR-098) |
| Cluster DNS off, or the proxy down | the egress proxy, at once: 503 "internal routing was not serving on this host … Turn cluster DNS on in Settings > DNS …, then stop and start the sandbox" |
| An address that no longer answers | 503 within 3 s, naming the address and the same remedy |

Nothing waits on a resolver or a dead listener.

### D6. Discoverability

- **Internal access card:** it lists sandbox callers next to project callers,
  and adds one by picking one of the user's sandboxes.
- **Sandbox page:** it lists every internal address the sandbox may call, with
  the exact `curl -sS http://<env>.<project>.temps.local/` line and, when the
  environment has its alias, `curl -sS http://<subdomain>.temps.local/`, from
  `GET /sandboxes/{id}/internal-addresses` (`alias_hostname`,
  `alias_curl_command`). Without any grant, it shows where to add one.
- **DNS Test resolution:** it can be asked as one of your sandboxes. It reports,
  for either name form, whether a grant admits the sandbox for that name, notes that the sandbox
  never looks the name up itself, and names the node whose proxy decides.
- **Refusals:** every egress refusal for a `.temps.local` name says what is
  missing and where to fix it.
- **The assistant:** asked to let a sandbox call a project, or shown its
  403, it proposes the grant for the person to confirm (D8).

### D7. An environment also answers at its subdomain

Every environment already has a unique subdomain (`<project>-<env>` by
default, unique among live environments, case-insensitively). Its public URL
is `<subdomain>.<preview domain>`. It is now also an internal address,
`<subdomain>.temps.local`, beside `<env>.<project>.temps.local`: a user who
knows the public URL can call the app internally by swapping the domain.

- **Same route, same decision.** The route table serves the alias with the
  same backends, project and environment as `<env>.<project>.temps.local`,
  so a caller is admitted by exactly the same rules: its own project, an
  ADR-101 grant, or a sandbox grant (D4). No new grant kind exists.
- **Published the same way.** The DNS publisher writes the alias next to the
  environment's name, pointing at the same internal proxies (every worker's
  bridge, or the control plane's gateway on a single server, ADR-104 D3), so
  project containers resolve it on every node. A sandbox never resolves it
  (D3).
- **Only one DNS label.** The alias exists only when the subdomain is one
  label (letters, digits, inner hyphens, at most 63). One label can never be
  another environment's `<env>.<project>` name or a name below one.

**Collision rule: the alias is the weakest name in the zone.** A one-label
name may also belong to a managed service (`orders-db.temps.local`), a node
or a static record. The alias always yields:

- The alias is published under its own registry owner,
  `environment_subdomain` (owner id: the environment), never under the
  environment's `environment` owner. A publish claims all its names or none
  (ADR-055 §10), so a shared owner would let a taken alias also freeze the
  environment's two-label name.
- Any other owner that publishes the alias's name takes it, whichever came
  first: the alias's claim and records are removed in the same transaction,
  and the alias's next publish is refused and recorded in
  `dns_name_conflicts`, where Settings > DNS reports conflicts. Among aliases,
  the first claim stays, but subdomains are unique, so two live
  environments never want the same alias.
- The route table, the DNS test and the sandbox's address list skip an alias
  whose name another owner holds. The DNS answer moves at once. A route
  table built before the other owner's claim keeps routing the alias to the
  environment until its next rebuild (on the next route change, and at least
  once a minute). Calls are still grant-checked during that window, and
  project containers already resolve the name to its new owner.

We considered and rejected letting the alias win, or the first claim win.
A database's name points at container addresses that change on restart. If
its publish were refused, the database's name would be stranded on a dead
address, the same reason the ADR-055 backfill ranks databases first.

### D8. The assistant proposes a grant; only its owner makes it

The AI assistant may propose, through `temps_write`, the four grant
operations: creating and deleting a sandbox grant or a project grant. This is
the vault-link path (ADR 103), not a new one. It reads the same lists the
console does (`list_internal_access_sandbox_grants`,
`list_internal_access_grants`, `list_sandbox_internal_addresses`), and the
`connect-a-service` playbook starts from the 403 a sandbox gets. The replayed
request runs through the same handlers, so the same rules hold:
`projects:write` and project scope on the target, owning the sandbox, seeing
the calling project, the 100-grant limit, and the audit entry written as the
person, with "via Temps AI assistant" in its user agent.

**Creating a grant needs the owner's confirmation of that exact action.** A
grant widens what a sandbox or a project can reach, so both create operations
join `HUMAN_CONSENT_OPERATIONS` with the vault session, link and bundle:

- The conversation's permission mode never approves them, Auto included. The
  chat waits for the person.
- The pending action runs only when `confirmed_by` is the user who owns it.
- Only that confirmation, made in the console, attaches `ConfirmedAiWrite` to
  the replayed request. Each create handler calls
  `require_human_confirmation`, which refuses an assistant request without
  it with 403 `HUMAN_CALLER_REQUIRED` before touching the database. The rule
  therefore holds however the operation is reached.

Deleting a grant only narrows reach. It needs no such consent, but a DELETE
is never approved automatically, so the person still confirms it.

**MCP.** The MCP server shares the assistant's write allowlist, so it exposes
the four operations the same way it exposes vault links. Its `confirm_action`
holds a proposal token, which is not the person's confirmation in the
console. A grant created through MCP is therefore always refused. Revoking
works once confirmed there.

We considered and rejected:

- **Keeping the operations away from the assistant, as before.** It left a
  sandbox agent stuck on a 403 that it could only describe.
- **Letting permission modes approve grants like other writes.** A sandbox
  runs code and reads pages that its owner never saw.

## Threat model

- **A sandbox forging its identity.** Identity is the source address of the
  egress proxy on the egress network. Docker assigns it, and every admitted
  request is checked against Docker's own view of that container. Headers,
  the `Host` name and the request body never say who calls. A sandbox has no
  way to originate a packet from another sidecar's address: it has no route
  to the egress network and no Docker access. A Compose deployment cannot
  impersonate the sidecar's name, labels or networks.
- **DNS rebinding.** Internal names are never resolved by the egress proxy,
  so no answer can redirect them. A public name whose answers include any
  private address is refused, as before. The internal proxy routes by
  `Host`, only to the backends in its own snapshot.
- **Reaching addresses that are not routes.** Only the two internal forms (one
  or two labels under `temps.local`) are forwarded, only on port 80 and only as plain http, and only to the
  internal proxy. The internal proxy serves only routes in its snapshot and
  answers 404 for anything else. Every other private, loopback, link-local
  or metadata destination, `.local` name or `localhost` stays refused in
  every mode.
- **A revoked grant taking effect.** Deleting a grant notifies
  `route_table_changes`. The control plane's feeder and the workers' long
  poll rebuild the snapshot within seconds, and at most a minute later
  through the safety refresh. The next request is refused with 403. The e2e
  run measures this.
- **A destroyed sandbox.** Destroying it deletes its grants in the same
  statement, through a trigger on `sandboxes.status`. The snapshot loader
  also ignores any grant of a destroyed sandbox. Its egress proxy is removed
  with it, and a container that later takes its address fails the Docker
  re-check. A stopped sandbox keeps its grants and has no running proxy.
  Moving a sandbox to another node, or renaming its container, re-announces
  the snapshot.

- **A prompt-injected agent granting itself access.** An agent in a sandbox
  acts with its owner's session. A page, file or command output can tell it
  to let its own sandbox call a project, and it may then propose that grant.
  Nothing it can produce creates the grant (D8): no tool argument, no
  proposal token, no MCP `confirm_action` and no permission mode. Only its
  owner's confirmation of that exact pending action, in the console, does.
  The proposal names the sandbox, the project and the environments, so a
  grant the owner did not ask for is visible before it exists.
- **An agent holding a raw API key.** A key can call the grant API directly,
  without the assistant. Such a call is the key's own, bounded by the key's
  scope and role and audited under it. D8 does not change that; it only keeps
  the assistant from creating grants without the owner.

## Consequences

- A sidecar created while internal routing was not serving refuses internal
  names until its sandbox is next started. The refusal says so. Recreating
  running sidecars when routing changes would cut their connections, so it is
  left to the next start.
- Sandboxes reach internal addresses only over plain http on port 80, which
  is all internal routes serve today.
- The route snapshot grows by one name per sandbox grant, bounded by the
  per-project limit and filtered per node.
- Each environment publishes one more DNS name and one more route (D7). A
  managed service created with an environment's subdomain as its name takes
  that alias; the conflict is reported, the two-label name keeps working.
