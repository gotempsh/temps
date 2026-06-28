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

Add a **`temps route`** command group that manages `custom_routes` directly
through `LbService`, mirroring the direct-DB style of `temps domain import`
(read `TEMPS_DATABASE_URL` from the environment; scrub it from argv):

| Command | Action |
|---|---|
| `temps route add -d <host> -u <host:port> [-t http\|tls]` | Create a route via `LbService::create_route` |
| `temps route list` (alias `ls`) `[--json]` | List all routes |
| `temps route show -d <host>` `[--json]` | Show one route (exact + wildcard match) |
| `temps route rm -d <host> [-y]` | Delete a route (confirms existence first) |

`--type http` is the default and terminates TLS at the proxy, so the hostname
still needs a certificate — the `add` command prints the exact `temps domain
add` / `temps domain import` follow-up. `--type tls` performs SNI passthrough
and needs no Temps-held cert (the upstream presents its own).

This is deliberately the **smallest coherent slice**: it exposes the existing
backend with validation (`host:port` parsing, port range, IPv6 bracket
handling) and a discoverable CLI, and changes no proxy data-plane behavior.

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

- **SSRF / authorization.** A route forwards `:443` traffic to an arbitrary
  internal `host:port`. The command is admin-only (DB access), but the console
  follow-up must add guardrails before exposing this to less-privileged roles.
- **Overlap.** `project_custom_domains` / `environment_domains` /
  `deployment_domains` cover Temps-*managed* deployments. `custom_routes` is the
  unmanaged-upstream escape hatch; the docs must make the distinction explicit
  so operators pick the right one.
- **CLI/server schema drift.** The `temps domain add`/`list` *API* commands
  have drifted from the running server ("error decoding response body"). The new
  `temps route` command sidesteps this by talking to the database directly (as
  `temps domain import` does), but the underlying API drift should still be
  fixed separately.

## Consequences

- Routing an unmanaged upstream becomes a supported, validated, scriptable
  operation instead of manual SQL.
- No data-plane change: the proxy already loads `custom_routes`; this only adds
  a writer/reader CLI in front of the existing `LbService`.
- The cert step remains explicit (`temps domain`) until follow-up #1 lands.
