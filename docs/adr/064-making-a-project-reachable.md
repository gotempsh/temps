# ADR 064: Making a project reachable: who first, then how

## Status

Proposed — 2026-10-24

Related: ADR-020 (every ingress serves every app), ADR 031 (managed DNS
records), ADR 080 (ingress is a node capability), ADR 088 (workers with no
public address), ADR 058 (ingress modes, providers, tunneled domains).

## Context

What a user wants after a deploy is one thing: their app at an address, for
the people who should use it. ADR 058 built the pieces: ingress modes,
providers (Cloudflare Tunnel first), tunneled domains and a per-hostname
publishing state. Putting them together is still the user's job.

What a new user goes through on a control plane whose workers have no
public address, which is every hosted plane and most home labs:

1. They join a worker and deploy a project. The deployment goes green.
   Nothing says that nobody can reach it.
2. To put it on a domain they must know, unprompted, that they need a
   tunnel; create a Cloudflare token with three permissions; add it as a
   DNS provider; run **Settings → Nodes → Expose your apps** (five steps);
   then go to **Add Domain** and pick "through the tunnel". That is about
   ten screens in three parts of the console, plus Cloudflare's.
3. A project's domains live under the project's **Settings**, next to build
   options and variables, although putting the app online is the main thing
   most users come to do.

And one thing cannot be expressed at all: **who** may reach the app. Every
way in is public. A user who wants an app only on their own network (a
tailnet, a LAN, a VPN) must build it by hand, and the obvious attempt fails
silently: a project IP allowlist of `100.64.0.0/10` behind a Cloudflare
Tunnel blocks everyone, because tunnel traffic carries the visitor's
internet address (`CF-Connecting-IP`), never a tailnet one.

Facts this decision relies on:

- The environment's container list
  (`GET /projects/{id}/environments/{env}/containers`) already returns each
  container's `node_name`, so where an environment runs needs no new field.
- A hosted control plane has no address of its own to hand out: its preview
  domain defaults to `localho.st` (ADR 058 D4), and Temps Cloud lists a
  managed subdomain as a later convenience. Today a public address needs the
  user's own domain.
- Cloudflare's dashboard accepts a token template link
  (`https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=…&accountId=*&zoneId=all&name=…`)
  that opens the create-token form with permissions filled in. The link
  cannot name a zone the user has not chosen yet, so it opens with all
  zones and the user narrows it in the form (D8).

## Decision

### D1. One flow, per environment: *Make it reachable*

There is one flow for putting a project's environment online, started from
the project, and it asks the questions in the order the user thinks them:

1. **Who should reach it?** Anyone on the internet, only my private
   network, or anyone who signs in (D6).
2. **At which address?** The hostname, e.g. `myapp.example.com`, or a name
   the chosen way in provides (a tailnet name, D5).
3. **Create.**

Temps works out the rest from what the cluster already has, and asks only
for what it cannot work out (a credential, a choice between two zones).
"How traffic gets in" is never a question by itself: it follows from the
audience and from what the workers have.

The flow is started from:

- the project header, which always shows the environment's state (D3) with
  **Make it reachable** while nothing reaches it;
- the project's Domains page (D2);
- the end of an environment's first successful deployment;
- the AI tools and the CLI, through the same API (D4).

The existing admin pages stay: **Expose your apps** for setting up ingress
for the whole cluster, the global **Domains** page, and **Add Domain** for
choices this flow does not offer (DNS-01 with a specific provider, an
uploaded certificate). They run the same operations underneath; this flow
is the shortest path through them, not a second implementation.

### D2. Domains is part of the project, not its settings

**Domains** moves from the project's Settings group to the project's main
navigation, next to Overview and Deployments. `settings/domains` and
`domains` keep resolving to it, so existing links keep working. The page
shows, per address, its audience, how traffic gets in, and the live state
from D3.

### D3. Every address says whether it is reachable, and why not

Reachability is computed per address, not per node. Under ADR-020 any
ingress serves any app and forwards over the mesh, so "the worker this
runs on has no ingress" is the wrong test. The right one is: does a way in
serve this hostname, and does DNS lead there?

`GET /projects/{id}/environments/{env}/reachability` returns, per address
attached to the environment:

| Field | Example |
|---|---|
| `address` | `myapp.example.com` |
| `audience` | `public`, `private`, `signed_in` |
| `way_in` | `tunnel:homelab`, `direct:edge-1`, `private:master` |
| `state` | `reachable`, `not_reachable`, `checking`, `unknown` |
| `reason` | "No worker takes traffic for this hostname." |
| `action` | what fixes it, with the link or command |

It builds on ADR 058's per-hostname state (`GET /ingress/hostnames`, D9)
and adds the direct and private ways in. With no address at all it returns
`state = not_reachable` and the plan D4 would propose, so the header can
say "Deployed on `master`, but nobody can reach it yet" with one button.

The header shows one line: **Public at `myapp.example.com`**, **Private at
`myapp.internal.example.com` (Tailscale)**, or **Not reachable yet**, with
the reason. A preview domain that only resolves locally (`localho.st`) is
never shown as reachable.

### D4. The plan is computed by the server and applied as one run

- `POST …/reachability/plan` takes `{audience, address?}` and returns the
  steps Temps would take and the fields still needed, in ADR 058 D8's
  descriptor form (`secret`, `dns_provider`, `managed_domain`, `text`), so
  the console renders it with no code per provider.
- **A plan is bound to the state it was computed from.** The plan response
  carries a `plan_id` and a fingerprint of everything the plan depends on:
  the environment's addresses and their audiences, the cluster's DNS
  providers and managed zones, the tunnels with their domains,
  `auto_publish` and hostname choices, the workers' ingress modes, and the
  current content of every existing DNS record the plan would change (its
  provider id, type, content and modification time, as read from the
  provider when planning). Plans are stored server-side and expire after
  15 minutes.
- **A DNS record edited after planning is never overwritten.** `apply`
  re-reads those records as part of the fingerprint check, and each "set
  DNS" step reads its record again immediately before writing and compares
  it with the plan; if another user or tool changed it in between, the step
  writes nothing and the run stops with "`myapp.example.com` was changed in
  Cloudflare after this plan was made (now `CNAME other.example.net`).
  Review the new plan." Cloudflare has no conditional update, so a change
  in the moment between that read and the write can still be lost; the
  window is one request long, and the overwritten value is kept in the run
  journal (the *before* state, see Undo below) so it can be restored.
- `POST …/reachability/apply` takes `{plan_id, answers}` and starts a run.
  It recomputes the fingerprint first; if anything changed since the plan
  (another user added a tunnel, a worker's ingress mode changed, a CLI run
  got there first), it creates nothing and answers `409` with the new plan,
  so a stale console tab, CLI call or AI tool never applies steps chosen for
  a world that no longer exists. Its steps report progress like **Expose
  your apps** does today (save the DNS provider, create the tunnel, publish
  the hostname, set DNS, issue the certificate, attach the domain, check
  reachability).
- **The run journal is written before each side effect, not after.** A
  provider call and a database row cannot commit together, so each step is
  recorded as `pending` with its idempotency key *before* the external call,
  and as `done` with the resource's id after it. The key is what finds the
  resource again: tunnels are named deterministically per run and install
  (`temps-<name>-<install>`, ADR 058), DNS records carry the ADR 031
  ownership marker plus the run id, and Temps-side rows reference the run.
  A step is retried with the same key, so a retry after a timeout finds the
  resource instead of creating a second one.
- **Incomplete runs are reconciled.** On start-up and every minute, a run
  with a `pending` step older than its timeout is resolved by looking the
  resource up by its key. For a step that **creates** something, found
  means it is recorded and the run continues or stops as the user asked.
- **For a step that changes an existing record, found is not enough.** A
  lookup by name and type finds the record whether or not the write landed,
  and may find a value someone else wrote since. Such a step is journaled
  with its *before* state (as read just before the write) and its intended
  *after* state, and a DNS write carries the run's ownership marker (ADR 031
  marker plus the run id) in the record. Reconciling it compares what is
  there now:
  - the intended *after* content with this run's marker: the write landed,
    the step is `done`;
  - still the *before* content: the write did not land; the step is retried
    through the normal pre-write check;
  - anything else: someone else changed it. The step becomes `conflict`,
    the run stops, and neither resume nor undo touches that record until a
    user decides ("`myapp.example.com` now points at `other.example.net`,
    which this setup did not write. Keep it, or let Temps set it again?").
- **A miss only counts as absence when the lookup can prove it.** A create
  that timed out may still have succeeded, and some provider listings lag
  behind writes. So each step kind declares whether its lookup is
  authoritative for absence, and after how long:
  - Temps' own rows: authoritative at once (same database, read after the
    step's transaction has ended).
  - Provider resources: authoritative only where the provider documents a
    read-after-write consistent lookup by the key, and only once the create
    request's own timeout plus a settle delay has passed, confirmed by two
    misses at least that delay apart. Which Cloudflare lookups (DNS records
    by name and type, tunnels by name) qualify is confirmed during
    implementation and recorded with the step kind.
  - Anything else: a miss is ambiguous. The step stays `pending`, the lookup
    is retried with backoff, and the create is never repeated while the
    result is ambiguous.

  Only a resolved step lets the run be undone or resumed. Until then the
  console shows "Recovering an interrupted setup" and which step it is
  waiting on, never a half-finished state as if it were complete; a step
  still ambiguous after an hour is shown with the resource to check by hand
  ("Check whether tunnel `temps-homelab-1a2b3c4d` exists in Cloudflare").
- **Undo** walks the journal newest first:
  - a resource the run **created** is removed, found by the same key; a
    resource the run found already there is never removed;
  - a record the run **changed** is put back to its *before* state, but only
    if it still holds exactly the run's *after* state. If it was changed
    again since, undo writes nothing to it, pauses, and reports the conflict
    with the record's current value, so a later edit by someone else is
    never overwritten. The user can then skip that record or restore it
    anyway, and the choice is journaled.
  - a step in `conflict` or still ambiguous blocks undo of everything before
    it, since earlier steps may depend on it, and the console says which.
  - **a tunnel the run created stays deleted.** ADR 058 recreates a tunnel
    that is missing at the provider for any connector that still exists,
    so undo never deletes the provider tunnel first. It first sets the
    connector (the ADR 058 `ingress_connectors` row, identified by its id
    and the provider tunnel id) to `deleting` and journals that; sync and
    reconcile skip a `deleting` connector entirely, so they never recreate
    or republish it. Before that, undo checks what the tunnel serves now:
    with `auto_publish` (ADR 058 D9) sync may have published hostnames on
    it that this run never journaled. If so, undo pauses as a conflict
    naming those hostnames, and the user either keeps the tunnel (undo
    skips it) or removes it anyway (journaled). Then the provider tunnel is
    deleted. DNS records go only through their own journaled steps: a
    record this run created is removed while it still holds the run's
    after-state and marker, a record it changed is restored through the
    before/after checks above, and no other record is touched by undo.
    Records ADR 058 sync published for the tunnel carry sync's marker, not
    the run's; sync's own cleanup for a removed connector deletes them, so
    none is left pointing at a deleted tunnel. The connector row is deleted
    last. If undo is interrupted in between, the
    reconciler finishes the deletion from the `deleting` state, never
    reverting it. A later run that wants a tunnel creates a new connector;
    a `deleting` one is never reused. A tunnel the run found already there
    is never deleted: undo only reverts the hostname choice the run made on
    it.
- The steps move from the console (`web/src/components/nodes/ingress-setup.ts`
  runs them in the browser today) to the server, so a closed tab cannot
  leave half a setup, and the CLI and AI tools get the same behaviour.

For the public audience the plan follows what exists:

| What the cluster has | What the user does |
|---|---|
| A tunnel that would publish the hostname (see below) | Create (1 action) |
| A tunnel whose domain covers it but would not publish it | Confirm "route `myapp.example.com` through tunnel `homelab`" (2) |
| A Cloudflare DNS provider, no tunnel | Confirm "create a tunnel on `master`" (2) |
| Nothing | Paste a Cloudflare token (D8), then Create (3) |
| A worker with `direct` ingress | Create; DNS set or shown (1–2) |

"Would publish" is ADR 058 D9's effective publication state for the
hostname, the same function `GET /ingress/hostnames` uses, not whether the
tunnel's domain covers it: an `ingress_hostname_choices` row wins over
automatic selection, and a tunnel with `auto_publish` off publishes only
the hostnames chosen for it. When the hostname would not be published by
the tunnel the plan picks, the plan says why ("tunnel `homelab` publishes
only chosen hostnames", "this hostname is set to tunnel `office`", "this
hostname is set to no tunnel") and adds an explicit step that records the
D9 choice. A choice another user made, for another tunnel or for none, is
never overridden without that step being shown and confirmed.

When several fit, the order is: an existing tunnel, an existing direct
ingress with DNS managed by Temps, a new tunnel. HTTP-01 is only offered
when a `direct` ingress takes port 80; it is never labelled recommended
otherwise.

### D5. Private network: a worker ingress that never listens publicly

A new worker ingress mode, `private`, next to ADR 058 D7's `off`, `fixed`
and `delegated`. Like those, it is the worker owner's decision, saved in
`agent.json`:

```
temps agent ingress private --listen tailscale|<IP>
temps join … --ingress private --listen tailscale
```

The private listener always uses port 443 (and 80 only to redirect to it),
so a private address is always a plain `https://<hostname>` URL and the
D7 check dials exactly what users open. A non-standard port is not offered:
it would have to be carried through the address, the routing, the checks
and every URL the console shows.

- **What it binds.** Only an address inside `100.64.0.0/10` (Tailscale and
  other CGNAT overlays), RFC 1918, or IPv6 ULA `fd00::/8`. `tailscale`
  resolves to the address of the `tailscale0` interface. The agent refuses a
  public address, `0.0.0.0` and `::`, with what to run instead. The heartbeat
  already reports the host's addresses (ADR 058 D7); the console marks the
  Tailscale one so the user picks it, not types it.
- **Addresses.**
  - *Your own domain*: Temps creates an A/AAAA record pointing the hostname
    at the worker's private address, through the cluster's DNS provider with
    the ownership marker (ADR 031), and issues the certificate with DNS-01.
    HTTP-01 cannot work here and is not offered.
  - *A tailnet name* (`myapp.<tailnet>.ts.net`): through Tailscale Serve,
    registered as an ADR 058 D8 provider with a Tailscale OAuth client stored
    with `EncryptionService`. No domain needed; Tailscale issues the
    certificate. Phase 3.
- **Audience is a property of the address, enforced in routing.** An address
  marked `private` is served only by `private` listeners. The tunnel
  publisher (`worker_public_routes`, ADR 058 D9) never publishes it, even
  under `auto_publish`, and `direct` listeners answer it with 404. DNS alone
  is not the boundary.
- **A second lock, per address.** Each private address carries its
  network's range (`100.64.0.0/10` for Tailscale, the chosen LAN range
  otherwise) in its route-snapshot entry, and the private listener refuses a
  peer outside it for that address only. It is not an environment-wide IP
  rule: an environment with a private address and a public one (tunnel or
  direct) keeps serving the public one to everyone. It is recorded on the
  run so undo removes it, and it is defence in depth: the boundary is the
  listener and the routing above. The private listener takes the client
  address from the TCP peer, never from a forwarded header.
- **Who in the network.** Temps does not manage Tailscale ACLs. The console
  says "everyone your tailnet's access rules allow to reach `master` on
  443".
- **Several audiences per environment.** An environment may have a public
  address and a private one at once; each address carries its own audience.

### D6. Signed-in audience

Anyone who signs in uses a public way in plus an access gate. Which gates
(the project's own protection settings, Cloudflare Access for tunneled
hostnames) is decided in a later ADR. This one reserves the audience value
and the flow step, so adding it is not a redesign.

### D7. Checks run from where they can tell

- **Public:** the control plane dials the hostname as today (ADR 058 D7's
  check, or the tunnel's publishing state).
- **Private, reachable from inside:** the control plane cannot reach a
  tailnet (a hosted plane runs in Temps Cloud), so the worker's agent makes
  the check, and it is bound to the environment's recorded private
  listener, not to whatever DNS says:
  - it resolves the hostname through its own resolver and compares the
    answer with the listener address recorded for that address; a mismatch
    is reported ("`myapp.internal.example.com` resolves to 10.0.0.9, not
    `master`'s 100.101.102.103") and nothing is dialled;
  - it then connects to the recorded listener address and port 443 only,
    with the hostname as TLS SNI and `Host`, verifies the certificate for
    that hostname, and follows no redirects;
  - it takes no host, address or URL from the API, only an address id the
    agent maps to its own route snapshot, so changing DNS cannot point the
    check at another service on the worker's network.

  The result is reported in the heartbeat.
- **Private, not reachable from outside:** the control plane resolves the
  hostname and confirms it points only at private addresses and that no
  tunnel record exists for it. Either failing is shown in red: "This
  private address is also published on the internet through tunnel
  `homelab`."
- Every failure names what to do, per the failure policy: missing DNS
  record, certificate not issued yet, the private listener not bound and
  why, the address resolving somewhere else.

### D8. The Cloudflare token step

- The step links to Cloudflare's token template URL with the permissions
  filled in: Zone · Zone · Read, Zone · DNS · Edit, Account · Cloudflare
  Tunnel · Edit. The user limits the token to one account and **one zone**,
  the one this address is in, creates it and pastes it.
- **The zone limit covers DNS, not tunnels.** Cloudflare Tunnel · Edit is an
  account-level permission with no narrower form for managing tunnels, and
  Temps needs it after setup too (publishing hostnames, rotating a tunnel's
  secret when a worker leaves, ADR 058). A stored token can therefore
  create, change and delete every tunnel in that account. The checklist
  says so next to the permission ("covers every tunnel in this Cloudflare
  account"), suggests a separate Cloudflare account for Temps when other
  tunnels live in it, and offers Cloudflare's optional client IP filter
  for the token when the control plane has a stable egress address. Temps
  itself only ever acts on tunnels it created (reused by stored id, never
  adopted by name, ADR 058).
- **A token reaching other zones is refused in this flow.** Temps lists the
  zones the pasted token can reach; if it reaches any zone other than the
  chosen one, the token is not saved and the message names the extra zones
  and says how to narrow the token. A DNS provider that deliberately
  manages several zones is still added from the DNS providers page, where
  an admin sees every zone it will manage before saving it.
- The permission keys for Zone and DNS are documented by Cloudflare. The key
  for Cloudflare Tunnel is verified during implementation; if there is none,
  the link fills in what it can and the checklist says what to add by hand.
- **Each permission is verified before anything is created, including
  write access.** Read calls cannot prove write access (an Edit permission
  and a Read one answer list calls the same way), so:
  - Zone · Read: the zone list above;
  - DNS · Edit and Cloudflare Tunnel · Edit: a create request with a
    deliberately invalid body. Cloudflare checks authorization before
    validation, so "not authorized" proves the permission missing and a
    validation error proves it present, and nothing is created. This
    ordering is confirmed against the live API during implementation and
    covered by a test with recorded responses.
  - If a probe cannot tell (an unexpected answer), the permission is marked
    unverified and the run is refused with "Temps could not confirm that
    this token can edit DNS in `example.com`. Check the token has Zone · DNS
    · Edit, or create a new one."

  The checklist marks each permission present, missing or unverified, and a
  run starts only when all three are present.

### D9. A tunnel and a private allowlist never combine

The IP access service refuses, with a specific message, an
environment-level allowlist that contains only private ranges when **any**
of the environment's addresses is served through a tunnel or
`behind_proxy` ingress, because every visitor on that address would be
blocked ("Visitors through tunnel `homelab` arrive with their internet
address, so `100.64.0.0/10` would block everyone on `myapp.example.com`.
To limit this app to your tailnet, make that address private instead.").
Environment IP rules apply to all of an environment's addresses, so there
is no case where such a rule is right for a proxied address; limiting one
address to a network is what D5's per-address range is for. The same check
runs when an address is later moved onto a tunnel while such a rule exists:
the move is refused with the same message, naming the rule.

## Security

- `private` ingress never binds a public address; the agent refuses one, and
  the control plane refuses to set one. The worker owner opts in, as for
  every other ingress mode.
- A `private` address is never published by a tunnel and never answered by
  a public listener (D5). This is enforced in the route snapshot, so a
  misconfigured DNS record exposes nothing.
- Changing an address's audience, in either direction, is a sensitive action
  (step-up): it changes who can reach the app. Private to public names what
  becomes public before it happens.
- A public DNS record pointing at a private address reveals that the name
  exists and the internal address. The console says so when the user picks
  their own domain, and offers the tailnet name, which publishes nothing.
- The agent's inside check dials only the private listener recorded for the
  address, follows no redirects, and takes no host, address or URL from the
  API (D7), so neither the API nor a DNS change can point it at another
  service on the worker's network.
- A tunnel that undo removes stays removed: its connector is marked
  `deleting` before the provider tunnel is touched, and ADR 058's sync never
  recreates or republishes a `deleting` connector (D4). Without that, sync
  would bring back a public hostname the user just took down.
- Undo deletes only what the run recorded as created. Credentials pasted into
  a run are handled as in ADR 058: `EncryptionService`, never returned,
  audited.
- A token is checked before anything is created, with calls that cannot
  create anything (D8), and one that reaches zones beyond the chosen one is
  not stored by this flow. That limits its **DNS** reach to the zone it
  serves. Its tunnel permission is account-wide (D8): if the stored token
  leaks, every tunnel in that Cloudflare account is exposed, which the
  console states before the token is saved.
- `apply` acts only on a current plan (D4), so a stale or replayed request
  cannot create resources the user did not see.

## Consequences

- A user who has deployed sees whether anyone can reach the app, and gets
  there in one to three actions from the project.
- Keeping an app on a tailnet or LAN becomes a supported, checked setup
  instead of something built by hand that fails silently.
- The ingress setup steps move from the browser to the server; **Expose your
  apps** becomes a client of the same runs.
- Self-hosted single-box installs are unchanged: their proxy serves every
  address publicly as today, and `private` is opt-in per worker.
- When Temps Cloud offers a managed subdomain, it becomes one more address
  source for the public audience, and the default when the user has no
  domain.

## Phases

| Phase | Scope |
|---|---|
| 0 | D2 Domains in project navigation; D3 reachability endpoint and the header state, linking to the existing setup pages |
| 1 | D1/D4 *Make it reachable* for the public audience: server-side plan, apply and undo; D8 token template link and token check; entry after first deploy; D9 |
| 2 | D5 `private` worker ingress with your own domain on the private address; D7 inside and outside checks |
| 3 | Tailscale Serve provider: tailnet names, and its identity headers for "only these tailnet users" |
| 4 | D6 signed-in audience |

## Open questions

1. Should phase 2 allow LAN ranges from the start, or only Tailscale until
   the inside check has been exercised on real networks?
2. Do preview environments inherit their parent environment's audience? A
   private app whose previews are public is a likely surprise.
3. Should the outside check for private addresses also run from Temps Cloud
   for hosted planes, so it does not depend on the control plane's own
   network?

## Implementation notes (phases 0 and 1)

Where the build differs from the text above, and why.

- **Journal keys.** DNS records carry the ADR 031 marker with the scope
  `{project, environment, controller "reachability"}` plus a signed
  `run_id` (added for the D4 review fixes; a marker without one is
  byte-for-byte what it was, so older markers still verify). The scope, not
  the run id, decides who may change a record; the run id tells a run's own
  write from anyone else's. A pending record is found again by its name and
  type at the provider, which is unique per environment. A
  custom-domain row is found by host and environment, a tunnel by its
  deterministic name, and the DNS provider by a name picked unused at apply
  time and stored in the run, so an interrupted run can always look up what
  it may have created.
- **Apply request.** `apply` takes `{plan_id, option, answers, confirmed}`.
  `option` picks one of the stored plan's options (the plan offers several);
  `confirmed` carries the explicit confirmation a plan option asks for when
  the tunnel choice for the hostname is elsewhere, off, or auto-publish is
  off (D4, ADR 058 D9).
- **Resume** continues a waiting or interrupted run from its journal. It
  does not recompute the fingerprint: the steps already done are recorded,
  and each remaining step re-checks its own preconditions.
- **Plans are rows.** `plan` stores a row under the project read permission
  so `apply` can bind to it; rows expire after 15 minutes and the
  reconciler purges them every minute.
- **D8 probe ordering is not yet confirmed against the live API** (no
  token was available while building). The recorded-response test pins the
  behaviour assumed: 401/403 or codes 10000, 10001 and 9109 mean missing,
  DNS code 9005 means present, and any validation error on the tunnel
  probe means present (the recorded tunnel body, code 1003, is an
  assumption to replace with a live capture). If Cloudflare ever validated
  before authorizing, the failure is still safe: the tunnel or DNS step is
  refused by Cloudflare, the run stops with that message, and undo removes
  what was created.
- **D8 link.** The template link fills Zone · Read and DNS · Edit; Cloudflare
  Tunnel · Edit is a checklist line the user adds by hand.
- **D9 scope.** The refusal covers IP access allowlists (environment and
  project), hostname moves onto a tunnel, and the reachability plan, which
  offers no tunnel while such a rule exists. Turning on a connector's
  auto-publish is not yet guarded; the next allowlist change or plan names
  the conflict.
- **Expose your apps** (`IngressSetupPage`) still runs its steps in the
  browser. It now uses the server token check; moving it onto server runs
  is left for when its own tests cover the run flow.
- **Phase 2 room.** `audience` is a column per address
  (`project_custom_domains`, `environment_domains`); a per-address network
  range for D5 belongs beside it.

## Implementation notes (phase 1 review fixes)

- **Which DNS records a plan holds.** Only the record a run itself writes:
  the direct ingress's `A`/`AAAA` at the address, in a zone Temps manages.
  Tunnel records are ADR 058 sync's (its own marker), not the run's, so they
  are not in the fingerprint. An existing record there that Temps does not
  manage blocks the managed-DNS option with "remove it there, or use another
  hostname": Temps never takes over a record, so a change step only ever
  edits one it already manages. A provider that cannot be read when planning
  blocks the option too ("try again in a minute") rather than planning blind.
- **Lookup authority.** Cloudflare documents no read-after-write guarantee
  for listing DNS records by name and type, nor tunnels by name. DNS steps
  therefore use the two-miss rule: 30 s (the create request's timeout) plus
  a 30 s settle delay, then two misses 30 s apart. A tunnel is never looked
  up at Cloudflare: its lookup is the `ingress_connectors` row, which is
  committed before Cloudflare is called (a create that fails later leaves
  the row, `CreateError::Saved`), so it is authoritative at once, like every
  Temps row. Lookups that cannot answer back off from 30 s, doubling to 15
  minutes; after an hour the run shows what to check by hand.
- **A changed record's reconcile** treats "still the before content" as a
  miss under the same two-miss rule before retrying the write, since a lagging
  read cannot be told from a write that did not land.
- **Conflicts** are answered through `POST …/runs/{run_id}/decide` with
  `{key, decision: keep|overwrite}`, journaled with the entry and audited
  (`REACHABILITY_CONFLICT_DECIDED`). Going forward `keep` skips the step and
  `overwrite` writes over the value there now (journaled as the new
  *before*); the run becomes `interrupted`, to resume. While undoing (state
  `undo_paused`) `keep` leaves it and `overwrite` restores *before*; undo
  carries on at once. A conflict found before any write (`unwritten`) has
  nothing for undo to put back and does not block it.
- **Undo and DNS records.** A record the run created is removed only while
  it still holds what the run wrote with the run's marker; gone already is
  done. "Remove anyway" is not offered for it: Temps never deletes a record
  it did not write. A changed record is restored with the content and the
  run id its *before* had.
- **Tunnels.** `deleting` is a value of `ingress_connectors.status` (a
  plain `VARCHAR`), so no migration was needed. Marking takes the lock that
  `ensure` and secret rotation write the row under, and the rotation error
  write never overwrites `deleting`, so no concurrent write can revert it.
  Sync refuses a `deleting` connector, `ensure` skips it (and deletes a
  tunnel it created meanwhile), reconcile, node specs and secret rotation
  leave it out, and every reconcile pass first finishes removing `deleting`
  connectors: sync's own records for it (a zone it cannot read now is
  retried, never skipped), then the provider tunnel when it is still the id
  undo recorded, then the row. A run picks tunnel names unused by any
  connector, `deleting` ones included.
- **A tunnel serving more than the run set up.** Before marking, undo asks
  the connector service what the tunnel publishes now (its publications and
  what the D9 plan sends through it). Hostnames the run never journaled
  pause undo as a conflict naming them. `keep` keeps the tunnel and, since
  it runs on them, the DNS provider and managed zone the run created for it;
  `overwrite` removes it anyway.
- **A tunnel that could not be removed** stops undo there (it stays
  `deleting` and the reconciler keeps removing it): the older steps, its DNS
  provider above all, are what removing it needs. Other failed removals are
  reported and the rest still attempted, as before.


## Implementation notes (phase 2)

- **Where the per-address bounds live.** `private_range`, `private_node_id`
  and `private_listener` sit next to `audience` on `project_custom_domains`
  and `environment_domains` (migration `m20261025_000001_private_audience`);
  `node_public_ingress.private_checks` holds the worker's inside checks.
  A public audience clears all three, so nothing stale routes the address
  later. The range is the private block of the listener's address
  (`100.64.0.0/10` on Tailscale), not a user choice in this phase.
- **Routing.** The route table withdraws every non-public address from
  every map the control plane's proxy serves (exact, HTTP/TLS routes,
  wildcards via `is_reserved`, the wake path) and from
  `worker_public_routes`, so neither the control plane, a hosted plane,
  a direct listener nor a tunnel ever answers it. It is sent, in the route
  snapshot's `private_routes`, only to the recorded worker, and only while
  that worker reports `private` mode bound on a private address
  (`private_listener_of`). Cluster-wide blocking policies hold private
  routes back as they do public ones. The private listener answers 404 for
  a host it has no private route for and 403 for a TCP peer outside the
  route's range; both carry `x-temps-private-ingress` (`no-route`,
  `outside-range`) so the inside check can tell the listener's refusal from
  the app's own answer. The range check runs before the port-80 redirect.
- **Bind state in the heartbeat.** `ListenerReport` carries the mode, the
  bound address, the error, `interfaces[].tailscale` and `private_checks`.
  The control plane refuses a private-mode report whose HTTPS listener is
  not on a private address (recorded as the listener error, naming the
  command) and never routes a private address to it. Each check is stamped
  with when the control plane received it, never the worker's time.
- **Inside checks** run on the agent after every change to its private
  routes and every 5 minutes, not on request from the control plane: that
  keeps the "only an address id" rule trivially true, since the control
  plane sends nothing at all. A check older than 20 minutes reads as
  `unknown` ("has not reported … for N minutes").
- **Outside check** (control plane): no tunnel publishes or is publishing
  the hostname, it resolves, every answer is private, and the recorded
  listener is among them. A failed outside check wins over a good inside
  one. The verdict carries `network` (`Tailscale` or the range) and the
  header reads "Private at … (Tailscale)".
- **Plan.** `private_worker` steps: attach, set audience (private + range
  + worker + listener), the A/AAAA record through a managed zone with the
  run's marker, the certificate over DNS-01, the checks. The audience is
  set before DNS points anywhere. No private worker: the plan names
  `temps agent ingress private --listen tailscale` and the worker to run it
  on (one the environment runs on, else the first active worker). No zone
  Temps manages, an unmanaged record at the name, or an environment on the
  control plane block the plan with the reason. The fingerprint (now `v3`)
  holds each private worker's listener, Tailscale flag and error, and the
  environment's private ranges.
- **Audience changes.** `SensitiveAction::ChangeAddressAudience {hostname,
  from, to}` guards `apply` when its option changes an existing address's
  audience (either way) and `undo`/`decide` when undo would put one back.
  A public plan for an address that is private now gets a `set_audience`
  step and a first warning naming what becomes public. Undo never restores
  the audience of an address the same run attached: that address is removed
  a step later, and it stays private until then.
- **D9 and `auto_publish`.** Turning a tunnel's `auto_publish` on runs the
  tunnel move guard for every served hostname it would then publish by
  itself (no choice made, it is the most specific automatic tunnel), and
  refuses with the guard's message.
- **A private-mode worker** refuses being enabled for public traffic from
  the console (409, naming `temps agent ingress delegated|fixed`), and its
  card shows the private listener, whether it is the Tailscale address,
  and the listener error.
- **Not in this phase:** a LAN range other than the listener's own private
  block, several private listeners per address, and the tailnet name
  (phase 3).
