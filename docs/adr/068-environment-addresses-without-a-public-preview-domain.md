# ADR 068: Environment addresses without a public preview domain

## Status

Proposed — 2026-10-25

Related: ADR 018 (stable environment hostnames), ADR 031 (managed DNS records
and the ownership marker), ADR 058 (ingress modes, D8 provider registry, D9
tunnel choice per hostname, D10 tunneled domains), ADR 064 (making a project
reachable: D3 per-address reachability, D4 plan/run journal, D7 checks),
ADR 065 (tailnet names).

## Context

Every hosted environment gets one managed environment domain when it is
created: an `environment_domains` row whose `domain` is the environment's
label (`environment.subdomain`, e.g. `hello-temps-production`), not a full
hostname (`EnvironmentService::create_environment`,
`reconcile_managed_domain`). Routing, reachability and monitors expand that
label to `<label>.<preview_domain>` (`PublicHostnameStrategy::environment_hostname`,
`route_table.rs`, `ReachabilityService::ways_in`).

`preview_domain` defaults to `localho.st` (`AppSettings::default`), a name that
resolves to 127.0.0.1. Until an operator sets a wildcard domain, every
environment's only generated address works on nobody's machine but the one
that opens it. Reachability already says so: `is_local_only` drops the address
from the ways in, and the header reads "Deployed on master, but nobody can
reach it yet: … only resolves to the computer it is opened on" (`compute.rs`).

Other surfaces do not ask reachability:

- **The uptime monitor** probes `<label>.<preview_domain>`
  (`MonitorService::populate_monitor_url` → `get_deployment_url_by_slug`).
  The outbound policy refuses loopback, so every check fails, and the project
  shows a red "Uptime down" dot for an app that is up behind a tunnel.
- **The open-in-new-tab button** opens `environment.domains[0]`
  (`resolveStableUrl` in `web/src/lib/deployment-url.ts`), which can be the
  loopback name.
- **A deploy reports success** with no word that nobody can open the result.
  Nothing records a warning on a deployment; the only per-install nudge is the
  "Add a wildcard domain" step of the setup checklist (`useGettingStarted.ts`).

Meanwhile an install can have a working way in that the environment domain
never uses: a Cloudflare tunnel (ADR 058). A connector knows its zone
(`ingress_connectors.domain`), and with `auto_publish` it publishes every
worker-served hostname under that zone (`ingress_publications`). But the
environment domain lives under `localho.st`, not under the tunnel's zone, so
it is never published. Users then add a custom domain per app by hand
(`app4.autoship.so`), and the environment domain stays a dead row.

Facts this decision relies on:

- A Cloudflare Universal SSL certificate covers the zone apex and one label
  below it (`*.example.com`), not deeper names. Temps already has the
  `Flat` hostname strategy for exactly this (`public_hostname.rs`).
- A tunnel publication writes a DNS record in the zone; ADR 031's ownership
  marker decides what Temps may change or remove.
- `production_hostname_template` and `environment_hostname_template`
  (ADR 058 D9) already shape generated labels (`{project}`, `{env}`).

## Decision

### D1. The environment domain is never lost

The managed environment domain row is the environment's identity, and it is
kept for the life of the environment whatever happens to addressing. Changing
`preview_domain`, adding or removing a tunnel, or adding custom domains never
deletes or renames it; only renaming or deleting the environment does, as
today. What changes is how that label is *served*: under the preview domain
(D2) and, when the preview domain cannot be reached, under a tunnel's zone
(D3).

### D2. A preview domain is usable or it is not

The control plane classifies `preview_domain`, from facts it already has:

| Status | Meaning | Source |
|---|---|---|
| `local_only` | `localhost`, `localho.st` or a name under them | `is_local_only` |
| `unresolved` | a probe label under it (`temps-probe-<random>.<preview_domain>`) does not resolve | DNS, as ADR 064 D7 |
| `no_way_in` | it resolves, but to no address a listener of this install serves publicly | ADR 064 ways in |
| `usable` | it resolves to a public way in of this install | ADR 064 ways in |

The status is shown next to the setting, and it is the "valid preview domain"
every later decision refers to. It is recomputed when the setting, the nodes'
public ingress or the tunnels change, and every 30 minutes.

### D3. Tunnel environment hosts: the environment domain under a tunnel's zone

When the preview domain is not `usable`, a tunnel can carry the environment
domain:

- **Opt-in per tunnel.** A new connector column, `environment_hosts`
  (default `false`), says "give every environment a hostname under this
  tunnel's zone". Existing connectors are unchanged. The console offers to turn
  it on in the D5 warning and when a tunnel is created on an install whose
  preview domain is `local_only`. At most one connector has it on.
- **The name.** `<label>.<connector.domain>`, where `<label>` is the
  environment's existing label (or `production_hostname_template` for
  production). One label below the zone, so Universal SSL covers it with no
  certificate step. The `Flat` rule applies to service hostnames
  (`{env}--{service}.<zone>`).
- **The row.** A second `environment_domains` row holding the full hostname,
  with a new column `generated_by_connector_id` naming the connector. That
  column is what marks it as Temps-generated: only those rows are renamed or
  removed by the rules below; a domain a user added is never touched.
- **Publication.** The host is published through that connector by the
  existing ADR 058 path: `auto_publish` connectors pick it up because it is
  under their zone; others get an `ingress_hostname_choices` row for it. DNS
  follows ADR 031: a name already held by a record Temps does not own is a
  conflict. The environment then gets `<label>-<hash>.<zone>` (the existing
  short-hash rule), and the console says why.
- **Lifecycle.**
  - Environment renamed: the tunnel host is renamed with it, new one published
    before the old one is withdrawn.
  - Environment deleted: unpublished and its record removed (owned records
    only).
  - `environment_hosts` turned off, or the connector deleted: the generated
    rows are withdrawn after the console lists them and the user confirms.
  - Preview domain becomes `usable` later: the tunnel hosts stay. A working
    address is never withdrawn by a change elsewhere. New environments stop
    getting one, and the console offers to remove the existing ones.
- **Preview (branch) environments** get one too: branch names become public
  DNS names and enter Certificate Transparency logs, which the opt-in says.
  `environment_hostname_template` can shorten them (open question 2).

Self-hosted installs that turn nothing on keep exactly today's behaviour: the
label under `localho.st`, nothing published.

### D4. One primary address per environment

Reachability (ADR 064 D3) names one **primary address** per environment: the
first reachable one, in this order:

1. a custom domain;
2. a tunnel environment host (D3);
3. the generated preview-domain host;
4. a private address (ADR 064 D5, ADR 065), for people on that network:
   any address that is not public, whatever serves it -- a private
   listener, a tailnet name through Tailscale Serve or a Tailscale Service,
   or a `tailnet_users` address that admits only the people listed.

Every surface that means "the app's address" uses it: the header, the
open-in-new-tab button, the deployment page, notifications, and the uptime
monitor. `environment.domains` in the API lists the primary address first.

With no reachable address because no way in was ever set up -- the
generated host only resolves locally or has no DNS record and no tunnel
covers it, its audience is one routing does not serve, or nothing is
deployed:

- the open button is disabled with the reason;
- the uptime monitor probes exactly as it did before this ADR, managed or
  created by hand: through this install's own proxy when the name resolves
  to this machine or the monitor is managed, out over the public internet
  otherwise. A plain local install on `localho.st` stays green when the app
  is up;
- `no_address` only replaces one failure: the probe's hostname does not
  resolve, and the environment was never given an address. That probe
  would have recorded a "DNS resolution failed" outage; it reports
  `no_address` ("nothing to check: nobody can reach this environment yet")
  instead, drawn grey, never `down`. Every check that succeeded before still
  succeeds, and every other failure (refused, timeout, 5xx) is still one;
- the monitor resumes on the next primary address without being recreated.

A way in that was set up and broke is an outage, not `no_address`: a tunnel
whose publication is in error, a worker or the control plane failing its
direct check, a custom or environment domain whose DNS record went away. The
monitor then probes the generated URL as before and records `down` or
`major_outage`, including a "DNS resolution failed" outage for a name that
no longer resolves.

A monitor whose check path was set by the user keeps that path on the new
address. A monitor created by hand with an explicit URL is not changed.

### D5. Warn when a deploy produces an app nobody can open

A successful deployment of a hosted environment carries a **reachability
warning** when, at completion:

- the environment has no reachable address (D4), **and**
- the preview domain is not `usable` (D2), **and**
- no tunnel environment host (D3) or custom domain serves it.

Excluded: projects hosted elsewhere (`hosting = external`), background workers
with no port, and environments with a reachable private address.

The warning never fails or pauses the deploy; the deployment status stays
successful. It appears in four places, all from one computed value (no new
table):

1. **The deploy log.** `mark_deployment_complete` writes a final block:

   ```
   ⚠ Deployed, but nobody can reach hello-temps-production yet:
     hello-temps-production.localho.st only resolves to this computer.
     Make it reachable:
       • publish it through tunnel 'autoship-so' as hello-temps-production.autoship.so
       • or set a wildcard preview domain in Settings → Platform
       • or add a domain to this environment
   ```

   The options listed are the ones that fit this install, in that order: a
   tunnel that could carry it (D3), the preview domain, a custom domain, and
   public ingress on a worker (with the exact `temps agent ingress` command)
   when no node takes public traffic.
2. **The deployment and project APIs.** A `reachability_warning` field
   (`null` when there is none) with the message and the fixes, so the console
   and `temps deploy` print the same words.
3. **The console.** A banner on the deployment page and the project header
   (which already shows the ADR 064 summary) with one button per fix. The
   tunnel fix runs the D3 opt-in through the ADR 064 plan and run, so it is
   undoable.
4. **One notification per environment** (`batch_key`
   `reachability:<environment_id>`), sent when the warning first appears and
   again only after it cleared and came back. No notification per deploy.

The setup checklist's "Add a wildcard domain" step becomes "Make your apps
reachable". It is done when the preview domain is `usable` or a tunnel has
`environment_hosts` on.

### D6. What is out of scope

- Callback and webhook URLs (`public_ingress_url`, `external_url`) keep their
  own setting and warning. A tunnel host for the console itself is a separate
  decision.
- How a managed service (Temps Cloud) supplies a preview domain to the
  installs it hosts is decided on that side. This ADR only requires that a
  supplied domain goes through D2 like any other.

## Security

- D3 publishes environment and branch names as public DNS names, and their
  certificates appear in Certificate Transparency logs. It is opt-in per
  tunnel, and the opt-in says so.
- D3 never overwrites a DNS record Temps does not own (ADR 031) and never
  touches an `environment_domains` row it did not generate
  (`generated_by_connector_id`).
- D3 names are predictable and published before anyone asks for them, so
  no other address may take one over. A custom domain is refused when an
  environment address of any project holds its name, and a generated name
  is refused when anything else in the install holds it. One hostname is
  never generated for two environments. Labels are a single ASCII DNS label,
  so a branch name cannot produce an IDN look-alike.
- Turning environment hosts on makes every environment public at once. It
  needs a recent MFA verification, and the confirmation is bound to the
  exact list of hosts that was shown.
- A tunnel environment host is public. Turning it on for an environment whose
  addresses are not public -- `private` (including tailnet names), `signed_in`
  or `tailnet_users` (ADR 064, ADR 065) -- is refused: those audiences are
  set per address, and a generated host must not widen them.
- The D4 monitor change adds no outbound request and removes none. The
  monitor probes exactly as before (through Temps' own proxy when it can);
  `no_address` only relabels a probe whose hostname did not resolve, and
  only when no way in was ever set up for any of the environment's
  addresses. It is not
  a way to silence a real outage: a way in that broke (a tunnel in error, a
  failed direct check, a custom domain losing its DNS record) keeps the
  probe and is recorded as an outage, and a local-proxy probe that fails is
  recorded as one too.
- The D5 warning carries hostnames and node names the viewer can already see.
  Notifications follow the existing preferences.

## Consequences

- A deploy never ends in a silent success that nobody can open.
- An install with a tunnel and no wildcard domain gets a working address for
  every environment with one setting, instead of a custom domain per app.
- The uptime dot stops reporting "down" for apps that are up but have no
  address it can probe.
- `environment_domains` gains rows holding full hostnames generated by Temps,
  marked by `generated_by_connector_id`; code that assumed every row is either
  a label or a user domain has to read that column.

## Phases

| Phase | Scope |
|---|---|
| 1 | D2 preview-domain status; D4 primary address in the header, open button, deployment page and API ordering; the monitor's `no_address` state; D5 warning in the log, the API, the console and notifications; the checklist step |
| 2 | D3 tunnel environment hosts: `environment_hosts` and `generated_by_connector_id` columns, generation, publication, renames, withdrawal, and the one-click fix from the D5 warning |

## Open questions

1. Should D3 also apply to environments created before the opt-in, all at
   once, or only on their next deploy? The proposal: all at once, listed and
   confirmed in the console first.
2. Do preview (branch) environments get tunnel hosts by default, or only
   production and named environments? Branch names leak into public DNS.
3. With several tunnels, is "at most one with `environment_hosts`" enough, or
   should a project choose its tunnel?
4. Should the D5 warning also fire when the only way in is private (a tailnet
   name), for an environment whose audience is `public`?

## Implementation notes

Recorded with the code that implements them. Where the text above is silent
or the code reads it a particular way, the choice is listed here.

### Phase 1

- **Only a sure answer warns.** The D5 warning and the D4 `no_address` state
  are set only when reachability settles on "not reachable" *and* no way in
  was ever set up for any address. Each verdict carries that as a flag
  (`nothing_set_up`, internal, not in the API), set only for a local-only
  host, an audience routing does not serve, nothing deployed, and the
  generated host with no DNS record and no tunnel covering it. While
  reachability is still checking, cannot tell (a DNS lookup failed), or a
  way in broke, the environment keeps today's behaviour: the open button
  links the stored domain, the monitor probes as before and records an
  outage if the probe fails, and no warning is written.
- **The monitor dials the primary address only when it differs from today.**
  A custom-domain or tunnel-host primary is probed over `https` through the
  public probe; a generated or private primary keeps the URL and routing the
  monitor already used, so an install that opts into nothing probes exactly
  what it probed before. The user's check path is appended to whichever
  address is used. There is no per-monitor explicit URL in the schema today
  (only `check_path`), so "a monitor created with an explicit URL is not
  changed" has nothing to apply to yet.
- **A local install keeps its health signal.** With no reachable address,
  every monitor probes exactly as before ADR 068: a managed one through the
  local proxy (`should_route_probe_locally`), one created by hand through
  the local proxy too once its name resolves to a non-public address. So
  "nobody else can reach it" never hides "the app is down" or "the app is
  up". The first cut of this change stopped probing instead, which turned a
  working local install's monitors grey; it now only relabels the outcome
  that was already a failure. The red dot that motivated D4 was a probe
  whose name did not resolve (an external URL, or a host with no record),
  and that `PublicProbeTarget::Unresolved` result, with `has_no_address()`,
  is the only case that records `no_address`. A failure to build the
  generated URL is still `degraded`, as before.
- **`no_address` is neutral everywhere.** It is excluded from uptime coverage
  and status buckets like the bootstrap `unknown` row, is skipped by outage
  detection (an open incident is neither resolved nor escalated by it, in
  the per-check path and in the periodic monitor scan alike), and the
  project dot draws it grey with "Nothing to check".
- **The notification episode lives on the notification row.** No new table:
  the newest notification under `reachability:<environment_id>` is the open
  episode until it is marked with `episode_cleared_at` in its metadata. The
  episode closes when a deploy completion finds the environment reachable,
  so a warning that clears and comes back notifies again on the next deploy.
  An episode no destination accepted (no provider configured, or every one
  refused it) is marked `episode_undelivered_at` and does not count as open:
  the next deploy that finds the environment unreachable sends it again, so
  "once per episode" never silently becomes "never".
- **D2 counts a tunnel zone.** A preview domain under the zone of a ready
  tunnel that resolves is `usable`: the tunnel carries it.
- **Which environment the project API warns about.** `ProjectResponse` has
  one `reachability_warning`: production when it is deployed, otherwise the
  lowest-id deployed non-preview environment. The deployment API carries the
  warning of the deployment's own environment, only for the current,
  completed deployment.
- **Lists use cached answers.** The deployment list attaches the primary
  address and warning only when reachability has a cached answer (one minute
  per environment, at most 1024 entries); single-deployment and project reads
  compute it. A list page never runs DNS lookups per row.
- **Generated rows.** An `environment_domains` row without a dot (a label) is
  reported with source `generated`, so the ordering can tell the generated
  host from a user-entered full hostname.
- **The console banner sits under the project header**, not inside its single
  row, next to the existing ADR 064 summary in the header. On the deployment
  page it replaces the ADR 064 "nobody can open it yet" alert when present,
  so the two never stack.
- **Notifications (D4 list).** Existing notifications that mention an address
  are not rewritten in this phase; the new reachability notification names
  the environment and its fixes.
- **The tunnel fix is offered only for worker-served environments.** A tunnel
  carries only what workers serve (ADR 058), so an environment whose
  containers run on the control plane is never offered "publish it through
  tunnel …"; it gets the preview-domain, custom-domain and worker-ingress
  fixes.
- **Generated API clients were refreshed offline.** No database or Docker was
  available while this was written, so the new and changed schemas were
  dumped from the plugins' OpenAPI documents, merged into the committed
  `apps/temps-cli/openapi.json` (canonical form checked with
  `bun run spec:check`), and both clients were generated from that file. A
  maintainer should run `bun run spec:update` against a live server before
  release, which should produce no further diff.

### Phase 2

- **The pass runs inside the ADR 058 reconcile.** Generated rows are brought
  in line with the environments at the start of every reconcile, under its
  lock: every minute, and whenever the peer table changes. The endpoint and
  the plan/run step only set the flag and ask for a reconcile, and so does
  turning an address anything but `public`, so a newly restricted
  environment loses its public host at once. Two passes can never run at
  once. Other renames and deletions are picked up within a reconcile tick,
  not synchronously.
- **One hostname, one environment.** Every name a generated row holds is
  claimed by its environment before any new name is chosen, so a duplicate
  label, or a renamed environment's old name kept until the new one is
  published, gets the other environment the short-hash name whatever order
  the environments are read in. When two rows already hold one name the
  oldest keeps it and the other is removed in the same pass. A partial
  unique index on `lower(domain)` for generated rows backs this up in the
  database.
- **No cross-project takeover.** A custom domain is refused when any
  `environment_domains` row holds its name, in any project, on create and on
  rename: the route table serves a custom domain in place of an environment
  address, and D3 names are predictable and published in advance. The other
  way round, each generated name is re-checked against custom domains,
  environment domains and status pages under the shared hostname lock right
  before its row is inserted. A name claimed since the pass began is left
  for the next pass, which gives it the short-hash name. Adding a tunnel
  environment host as the environment's own domain is refused too, because
  opting out would delete it.
- **Labels are one ASCII DNS label.** The environment's label is sanitised
  as `environment_label` does it: anything but ASCII letters and digits
  becomes a hyphen, so dotted legacy subdomains stay one level below the
  zone and no IDN look-alike is published. Labels over 63 characters are cut
  with a short hash. The full host is then checked against the tunnel's zone
  before a row is written.
- **Publication goes through the ADR 058 plan, not a separate path.** A
  generated row counts as a hostname chosen for its tunnel, and so do its
  Flat service hosts. A choice the operator made for the same hostname wins.
- **Ownership (ADR 031) is checked on A, AAAA and CNAME records.** A name
  with no record, with this install's record, or with only this install's
  orphaned marker is free. Any other answer for any of the three types gets
  `<label>-<hash>` (an unmanaged record, another install's record or marker,
  a registry conflict). So does a name another address in this install
  uses: a custom domain, an environment domain, a status page, or a name
  Temps reserves for itself (the console, the public ingress, the preview
  apex). If a lookup fails, the natural name is tried and publication
  reports a conflict if there is one. The listing's `note` says why a host
  has the hashed name.
- **New hosts are bounded per pass.** A pass resolves at most 50 new names
  (each needs a DNS ownership lookup). Larger installs converge over a few
  ticks; existing hosts are never re-checked.
- **Service hosts use the Flat form under the tunnel's domain:**
  `<host label>--<service>.<tunnel domain>`, added to the route table only
  where no route already claims the name. When `<label>--<service>` is longer
  than one DNS label, the shortened, hashed form is routed but not mapped
  back to its tunnel, so it is not published. This is left for a follow-up.
- **Refusals.** An environment with any address that is not `public`
  (`private`, including tailnet names through Serve or a Tailscale Service,
  `signed_in`, `tailnet_users`, or an audience a newer version wrote) gets no
  generated host. The opt-in lists it under "Not published" with the
  reason, and the D5 warning offers it no tunnel fix. Projects hosted
  elsewhere and deleted environments are refused the same way.
- **Confirmation is bound to the list shown.** The `GET` listing carries a
  `confirm_token`, a hash of the tunnel, its on/off state and every listed
  environment's host. `PUT …/environment-hosts` needs it back as `confirm`,
  in both directions. A missing token is a 400. A token for a list that has
  changed since it was shown is a 409, and the console reloads the list.
- **Turning it on is a sensitive action** (`publish_environment_hosts`),
  both on the endpoint and through the `tunnel_environment_hosts` plan
  option, because it makes every environment public at once. Turning it off
  is not. When two tunnels race to turn it on, the loser gets a 409 from the
  partial unique index, not a 500.
- **Opt-out and connector deletion.** Opting out deletes the generated rows
  in the same transaction as the flag.
  Deleting a connector removes its rows by foreign-key cascade, and the
  console's delete confirmation lists them first.
- **The one-click D5 fix is a plan option,** `tunnel_environment_hosts`,
  with a `publish_environment_hosts` step journalled as an
  `environment_hosts/<connector>` intent. Undo turns the flag off, which
  withdraws every generated host, not only this environment's: the switch is
  per tunnel. A run that finds the flag already on records nothing, so its
  undo leaves the hosts to whoever turned them on.
- **The D5 tunnel offer follows the generated name.** It uses the production
  template for production, and offers only the tunnel that already has
  environment hosts on when one does (Q3).
- **Q2 (branch environments) is implemented as accepted.** They are
  included, and both the opt-in dialog and the plan's confirmation warn that
  branch names become public names, with their certificates in
  Certificate Transparency logs.
- **Not run here:** the migration test needs Docker and skips at runtime on
  this machine; CI runs it.
