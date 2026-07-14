---
title: "ADR-020: First-class reverse-proxy routes"
status: Proposed
date: 2026-06-27
author: Ben Herila
---

# ADR-020: First-class reverse-proxy routes

**Status:** Proposed
**Date:** 2026-06-27
**Author:** Ben Herila

## Context

Temps owns `:443` on each host it runs. Operators frequently need that same
listener to front a service Temps does **not** deploy or manage — a side-car
that runs in its own Docker Compose stack, a self-hosted object store, a legacy
app on a fixed localhost port. The primitive for this already exists:

- **`custom_routes`** (`crates/temps-entities/src/custom_routes.rs`) — a row
  mapping `{ domain, host, port, enabled, route_type }`. `route_type` is
  `http` (terminate TLS at the proxy, match on the HTTP Host header — Layer 7)
  or `tls` (match on TLS SNI and pass through without terminating — Layer 4).
- **`LbService`** (`crates/temps-proxy/src/service/lb_service.rs`) — full
  `create_route` / `get_route` / `list_routes` / `update_route` /
  `delete_route` / `get_route_by_host`, including wildcard matching.
- **`route_table.rs`** loads every `enabled = true` custom route into the live
  proxy; these rows carry `None` for project / environment / deployment.
- **TLS** is served from `tls_acme_certificates` (encrypted `private_key`) for
  `domains` rows. `temps domain add` (ACME) and `temps domain import` (custom /
  self-signed) already populate that table.

What is missing is **a supported surface**. Configuring one of these routes
today means hand-writing an `INSERT` into `custom_routes` and, for an HTTP
route, separately importing a certificate. This is exactly what was required to
put `sentry.careowner.com → 127.0.0.1:9300` behind Temps on devprod0: a manual
`custom_routes` row plus a `temps domain import` of a self-signed origin cert.
The `staging-t-files → rustfs:9000` route was set up the same way. Both are
second-class, DB-surgery paths with no UX, no validation, and no discoverability.

## Decision

Add a **`temps route`** command group that manages `custom_routes` through the
authenticated load-balancer API (`TEMPS_API_URL` + `TEMPS_API_TOKEN`). Going
through the API preserves RBAC, validation, typed HTTP errors, and audit logs;
the CLI never receives database credentials:

| Command | Action |
|---|---|
| `temps route add -d <host> -u <host:port> [-t http\|tls]` | Create a validated and audited route |
| `temps route list` (alias `ls`) `[--json]` | List all routes |
| `temps route show -d <host>` `[--json]` | Show one exact route |
| `temps route rm -d <host> [-y]` | Delete one exact route |

`--type http` is the default and terminates TLS at the proxy, so the hostname
still needs a certificate — the `add` command prints the exact `temps domain
add` / `temps domain import` follow-up. `--type tls` performs SNI passthrough
and needs no Temps-held cert (the upstream presents its own).

Route domains are canonicalized and protected by a normalized unique index.
Wildcard overlaps are rejected, and managed-domain collisions require the
explicit `--force-override` acknowledgement. Private or loopback upstreams
require `--allow-private-upstream`; link-local, metadata, unspecified,
broadcast, and multicast targets remain blocked even with that flag. IPv6
upstreams are stored bracketed so socket address rendering remains valid.
Hostname upstreams must resolve when the route is written and are persisted as
a validated literal address. The data plane therefore never re-resolves an
operator-supplied hostname after the SSRF check.

The override decision is durable: `force_override` is stored with the route.
On every route-table reload, all managed names are generated first—including
environment aliases, Compose service names, internal names, and deployment
fallbacks. They take precedence over an overlapping custom route unless that
custom route carries the explicit flag. This also protects managed domains
created after the custom route.

The hardening migration disables legacy hostname and special-use-address
upstreams that could not have passed the new validation. The route loader
independently repeats literal-address validation so direct database writes
cannot reintroduce an unsafe route.

### Naming

The route command is a **new top-level `temps route`**, not a subcommand of
`temps proxy`. `temps proxy` is already the standalone proxy *serve* process
(ADR-017) and takes serve flags directly; overloading it with route CRUD would
be a breaking change to that command's argument surface.

## Follow-ups (out of scope for the first slice)

1. **Cert convenience.** A `--self-signed` flag on `route add` that generates
   an origin cert (workspace already depends on `rcgen`), encrypts the key with
   the data-dir `EncryptionService`, and writes the `domains` row — collapsing
   the two-step route+cert dance into one command for the HTTP case.
2. **Web console UX.** A "Reverse Proxy" create form in `web/src` (domain,
   upstream(s), TLS mode, health-check path) surfacing upstream health, which
   `route_table` already probes.
3. **`update`/`enable`/`disable`.** `LbService::update_route` already exists;
   expose it once the add/list/show/rm surface settles.
4. **First-class project type.** Optionally promote a route to a real
   `ProjectType::Proxy` (today `Server | Static`) so it has status rather than
   being an orphan `custom_routes` row.

## Risks & open questions

- **Internal exposure.** A route can deliberately publish a private upstream.
  It requires `LoadBalancerWrite` plus `--allow-private-upstream`, prints a
  warning, and records the acknowledgement in a fail-closed audit intent before
  the mutation. Special-purpose destinations such as cloud metadata addresses
  cannot be enabled. DNS failures are rejected instead of bypassing validation.
  Carrier-grade NAT, benchmarking, documentation, reserved, and other
  non-global special-purpose ranges are also hard-blocked; only RFC1918/ULA and
  loopback can be admitted with the explicit acknowledgement.
- **Overlap.** `project_custom_domains` / `environment_domains` /
  `deployment_domains` cover Temps-*managed* deployments. `custom_routes` is the
  unmanaged-upstream escape hatch; the docs must make the distinction explicit
  so operators pick the right one.
- **CLI/server schema drift.** Route request DTOs reject unknown fields and the
  CLI reports the server's Problem Details body when versions disagree.

## Consequences

- Routing an unmanaged upstream becomes a supported, validated, scriptable
  operation instead of manual SQL.
- Managed domains win over non-forced custom routes at every data-plane reload;
  explicit overrides remain effective because their intent is persisted.
- The cert step remains explicit (`temps domain`) until follow-up #1 lands.
