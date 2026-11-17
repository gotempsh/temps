# ADR-082: Path routes between services of a project

> **Renumbered:** this ADR was numbered 050 until 2026-10-31, when duplicate ADR numbers were removed. Older references to "ADR-050" may mean this document; the [ADR index](./README.md#renumbered-adrs) lists every renumbered ADR.

**Status:** Proposed
**Date:** 2026-09-28
**Related:** ADR-081; `crates/temps-project-groups/src/routes.rs`; `crates/temps-routes/src/route_table.rs`; `crates/temps-proxy/src/proxy.rs`

---

## Context

A project (`project_groups`) gathers services (`projects`). Every service gets its
own hostnames, and until now the proxy routed purely by host: one hostname, one
service.

Many real apps assume one origin. Their frontend calls a same-origin `/api`: a
Vite or Next.js app deployed statically next to its API, or a monorepo like
Infisical. Deployed as two services, that frontend calls `/api` on its own
hostname and gets its own `index.html` back (a 200 from the SPA fallback), or a
404. The workarounds all push work onto the user:

- CORS plus a build-time API URL.
- A reverse proxy container of their own.
- A custom domain per service.

Both modes must keep working:

- **Domain per service** (today). Each service keeps its own hostnames. Nothing
  changes for services with no routes.
- **One origin per project**. A service's hostnames hand selected path prefixes
  to other services of the same project.

The proxy's hot path must stay in memory, with no database access per request.

## Decision

### Data model

```
project_group_routes
  id, group_id → project_groups (CASCADE)
  source_project_id → projects (CASCADE)   -- whose hostnames receive the request
  target_project_id → projects (CASCADE)   -- who serves it
  path_prefix VARCHAR(255)                 -- normalized: /api, /api/v2
  strip_prefix BOOL DEFAULT FALSE
  UNIQUE (source_project_id, path_prefix)
  CHECK source <> target, prefix starts with '/', no trailing '/'
```

- **A rule belongs to the source service, not to a hostname.** It applies to
  every hostname of every environment of the source: the environment alias,
  custom domains, and preview hosts. Adding a domain later needs no extra step.
- **Environments pair by slug.** `production` goes to `production` and `staging`
  to `staging`.
- **Previews pair by branch.** A preview goes to the target's preview of the same
  branch. There is no fallback: a preview frontend must never quietly call the
  production API. With no match, the proxy answers 502
  `path_route_unavailable`. The API reports each environment's pairing
  (`environments[].target_environment_id`, `target_deployed`) so the console
  can show it before anyone hits it.
- **Rules follow the target's current deployment.** Hostnames of older,
  still-running deployments stay routable on their own, but a path route always
  follows what the target environment serves now.
- **Membership is enforced in the database.** A trigger on
  `project_group_members` deletes the rules involving a project when it leaves
  (or moves out of) its group, whatever code path moved it.

### Prefixes

- **Normalization.** Input is normalized (`api/` becomes `/api`). Segments are
  limited to unreserved URL characters (`A-Z a-z 0-9 - . _ ~`), and `.`/`..`
  are rejected. A prefix therefore means the same thing to every client, and
  percent-encoding cannot smuggle a different path past the match. The table's
  CHECK constraint enforces the same allowlist, so no code path can store
  anything else.
- **Whole-segment matching.** `/api` matches `/api` and `/api/users`, never
  `/apiary`. The longest prefix wins.
- **Platform paths are reserved.** A rule cannot claim a path the platform
  answers on every app hostname: `/_temps`, `/api/_temps`, `/api/__temps`,
  `/api/otel` and `/.well-known`. The proxy never path-routes a request under
  them either, so `/api → backend` still leaves `/api/_temps` (analytics
  ingest) to the platform.
- **Stripping is off by default**, because most backends expect the path they
  were called with. When it is on, `/api/users?x=1` is forwarded as
  `/users?x=1`, and `X-Forwarded-Prefix: /api` tells the target where it is
  mounted. The forwarded path always starts with exactly one `/`, so
  `/api//evil.com/x` becomes `/evil.com/x`, not `//evil.com/x`. A target
  building a redirect from that path would otherwise send the user to another
  host. On routed requests, a client-supplied `X-Forwarded-Prefix` is always
  dropped.
- **Redirecting hostnames redirect every path.** For example, `www` → apex
  redirects `/api` too.

### Proxy: in memory

`CachedPeerTable::load_routes()` already rebuilds a whole `RouteTableSnapshot`
and swaps it atomically (ArcSwap). Rebuilds are triggered by PG NOTIFY on
`project_route_change`. A new pass, `load_path_routes`, runs once per rebuild:

1. Load all rules, plus the environments of the projects they involve (two
   queries).
2. Index the new routes by environment id, keeping only each environment's
   current deployment.
3. For every source environment, pair a target environment and attach its
   `RouteInfo`. The result is `path_routes: HashMap<env_id, Arc<[PathRoute]>>`,
   longest prefix first. A target with no deployment is stored as `target: None`.

Per request, `match_path_route(host, path)` does the following:

- one snapshot load;
- an early return when no project uses path routes;
- a reserved-path check;
- the host lookup the proxy already does;
- one hash lookup by environment id;
- a scan of that environment's handful of prefixes.

It does no I/O and takes no locks. The match is resolved once in
`request_filter` and kept in `ProxyContext::path_route`. Everything downstream
reads it:

- **Project context** is the target's project, environment and deployment, so
  attack mode, the password gate, connection limits, timeouts, analytics and
  proxy logs are the target's.
- **Static serving** serves the target's files if the target is static;
  otherwise it skips the source's static files.
- **`upstream_peer`** round-robins the target's backends.
- **`upstream_request_filter`** strips the prefix when asked.

The rules table has a trigger that notifies `project_route_change`, so a new
rule applies within the reload latency, without a redeploy. A deploy of the
target already triggers a reload, which re-points rules at the new deployment.

### API and clients

```
GET    /project-groups/{id}/routes
POST   /project-groups/{id}/routes        { source_project_id, path_prefix, target_project_id, strip_prefix }
DELETE /project-groups/{id}/routes/{route_id}
```

- **Reading** requires `ProjectsRead` and a visible group. Rules involving a
  project hidden from the caller are left out.
- **Writing** requires `ProjectsWrite` on both source and target. Adding a rule
  changes the source's traffic and sends new traffic to the target.
- **Changes are audited** as `PROJECT_GROUP_ROUTE_ADDED` and
  `PROJECT_GROUP_ROUTE_REMOVED`.

The CLI has `project-groups routes list|add|remove`, and the console has a Routes
section in the project's Settings.

## Consequences

- An app that expects one origin deploys as separate services with no CORS and
  no custom proxy. Services keep their own hostnames as well.
- **Protections of the target apply.** A request under `/api` on the frontend's
  host is governed by the backend's password gate, attack mode and limits: it
  reaches the backend exactly as if it had used the backend's own hostname. A
  frontend's password therefore does not protect a routed API. Protect the
  target too.
- **Sleeping targets are not woken by a path route.** An on-demand target that
  is asleep has no route in the snapshot, so its rules answer 502 until it is
  woken through its own hostname. Waking through path routes can come later.
- **Cookies are per origin, not per service.** Under one origin, the frontend
  and the API share cookies. That is usually why people want one origin, but it
  also means the backend sees the frontend's cookies.
- The TLS certificate is unchanged, because path routing happens after the
  handshake on a hostname that already has one.

## Alternatives considered

- **Rules per hostname or domain.** More flexible, but every new domain would
  need its rules re-entered, and preview hosts would have none. Rules per
  source service cover the actual need.
- **A "project domain" entity owning the routes.** It would introduce a new
  first-class hostname concept and a migration of existing domains. Rules on
  the source service reach the same result with one table.
- **Resolving the target per request.** It would mean a database query or a
  second cache with its own invalidation. Resolving at reload time reuses the
  table's existing invalidation, and the per-request cost is a hash lookup.
