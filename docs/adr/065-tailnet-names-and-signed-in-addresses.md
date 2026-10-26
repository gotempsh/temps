# ADR 065: Tailnet names, tailnet users and signed-in addresses

## Status

Proposed — 2026-10-25

Related: ADR 031 (managed DNS records and the ownership marker), ADR 058
(ingress modes, the D8 provider registry, D9 publication state, connectors),
ADR 064 (making a project reachable: D1 audience → address → Create, D4
plan/apply/run journal, D5 private ingress, D6 signed-in audience, D7
checks, D8 provider tokens, D9 tunnels and allowlists).

## Context

ADR 064 decided the flow (who, then where, then Create), the run journal
and the `private` audience on a worker's own private listener. It left two
phases open:

- **Phase 3:** a tailnet name (`*.ts.net`) as the address of a private app,
  through Tailscale Serve, and "only these tailnet users" as an audience.
- **Phase 4:** the signed-in audience (D6), which ADR 064 reserves and the
  plan refuses today ("Addresses for people who sign in are not available
  yet").

This ADR is the design for both. It extends the phase 2 code rather than
adding a second path: `PrivateFacts`, the inside and outside checks, the
`private_range`/`private_node_id`/`private_listener` columns, the
`private:<node>` way in and `network: "Tailscale"` all stay, and the
Tailscale ways in are new values beside `private_worker`.

Facts this decision relies on. Each was checked against Tailscale's
documentation, its source at release v1.104.2, its published OpenAPI spec,
or Cloudflare's documentation, as cited.

**Tailscale Serve and Services**

- Serve gives a machine one name, `<machine>.<tailnet>.ts.net`, and proxies
  HTTPS on it to a local backend. Proxy targets are `http://127.0.0.1:<port>`
  ("only `http://127.0.0.1` is supported for proxies") and, since v1.94.0,
  `unix:<path>` sockets. The socket target is absent from `ipnlocal/serve.go`
  at v1.92.0 and present at v1.94.0. Setting a socket target needs root,
  or an operator who can `sudo`; the LocalAPI refuses it otherwise ("must
  be root, or be an operator and able to run 'sudo tailscale' to serve a
  path or Unix socket"). [kb/1242], [kb/1312], [src: localapi/serve.go]
- Serve on port 443 of a machine's own Tailscale address is handled inside
  tailscaled: netstack intercepts TCP to a served port
  (`ShouldInterceptTCPPort`). A kernel listener on the same address and port,
  such as phase 2's private listener on `tailscale0:443`, no longer gets
  that traffic. [src: wgengine/netstack/netstack.go]
- **Tailscale Services** give an app its own name,
  `<service>.<tailnet>.ts.net`, and an inbound-only virtual IP (TailVIP).
  The host runs `tailscale serve --service=svc:<name> …`, which configures
  Serve for that Service and advertises it. Requirements: hosts "must use a
  tag-based identity" (a device signed in as a user cannot host one);
  "an Admin, Network admin, or Owner must approve the host before it
  becomes active", unless the tailnet policy has
  `"autoApprovers": {"services": {"svc:<name>": ["tag:…"]}}`; hosts need
  v1.86.0 or later; Linux clients on 1.93 or earlier need accept-routes to
  reach a Service; TCP only. Services can be a grant destination
  (`"dst": ["svc:web-server"]`). [kb/1552]
- **The Tailscale API manages Services:** `GET/PUT/DELETE
  /tailnet/{tailnet}/services/{serviceName}` (scopes `services:read` /
  `services`; PUT creates or updates), `GET …/services/{name}/devices`
  (lists hosts with `approvalLevel` `not-approved`, `approved:auto` or
  `approved:manual`) and `GET/POST …/services/{name}/device/{deviceId}/approved`.
  The last two list `services` and `devices:core` as their scopes. A
  Service has a `name` (`svc:…`), `addrs`, `ports` (`tcp:443`), `tags` and
  a free-text `comment`. The Services API has no ETag or `If-Match`; only
  the policy file does. [Tailscale OpenAPI spec]
- **OAuth clients (trust credentials)** are scoped. `devices:core`, the
  write scope, can "authorize or remove [devices], and change their tags",
  and a credential with it must be created with tags. The trust-credentials
  page's scope table does not list `services` yet; the API spec does.
  [kb/1623], [Tailscale OpenAPI spec]
- **Certificates.** Serve needs HTTPS certificates turned on for the
  tailnet, which in turn needs MagicDNS. tailscaled requests the
  certificate from Let's Encrypt itself. Certificates are logged in public
  Certificate Transparency logs, and Tailscale warns "Do not enable the
  HTTPS feature if any of your machine names contain sensitive
  information." [kb/1153], [kb/1312]
- **Identity headers.** For tailnet traffic Serve sets
  `Tailscale-User-Login`, `Tailscale-User-Name`, `Tailscale-User-Profile-Pic`
  and `Tailscale-Headers-Info`, after deleting any copies the client sent.
  It sets none for traffic from tagged devices ("Not setting identity
  headers for tagged nodes"), none for Funnel traffic (which gets
  `Tailscale-Funnel-Request: ?1` instead), and none when its own `WhoIs` of
  the source finds nothing. Values may be RFC 2047 Q-encoded. Users on
  devices shared into the tailnet do get headers, with their own login.
  Serve also sets `X-Forwarded-For` to the tailnet source address and
  `X-Forwarded-Host` to the requested name, and for a socket target it
  rewrites `Host` to `localhost`. [kb/1312], [src: ipnlocal/serve.go]
- **The LocalAPI** on tailscaled's socket (`/var/run/tailscale/tailscaled.sock`
  on Linux) has what an agent needs locally: `GET/POST serve-config` with an
  `Etag` on read and `If-Match` on write (a mismatch is `412`), prefs (the
  CLI advertises Services by editing `AdvertiseServices`), `status`
  (`BackendState`, `CertDomains`, `CurrentTailnet.MagicDNSEnabled`,
  `Self.DNSName`, `Self.Tags`) and `whois?addr=<ip[:port]>` (the node with
  its tags and the user profile). Tailscale does not document the LocalAPI
  as a stable public interface. [src: ipn/localapi, ipn/ipnstate]
- **Funnel lives in the same Serve config** (`AllowFunnel` per name and
  port). One `tailscale funnel` command makes a Serve name public.
  [src: ipn/serve.go]

**Gates for people who sign in**

- **Temps' password protection** (`deployment_config.security.password_protection`)
  is one shared password per environment or project, a bcrypt hash and a
  7-day HMAC cookie `_temps_pw` keyed on that hash, set with `Domain=<host>`
  (which also sends it to subdomains). Any `security` setting makes a route
  not worker-servable (`worker_servable` in `temps-routes`), so only the
  control-plane proxy enforces it. On a hosted plane, whose proxy is off,
  a password-protected app is not reachable at all.
- **Temps is an OIDC client, not a provider.** Console users sign in with
  password and MFA or through configured OIDC providers (`oidc_providers`).
  Project access is through teams (`project_team_access`; a project with no
  rows is open to every user with the instance-wide permission).
- **Cloudflare Access** puts a sign-in in front of a hostname at Cloudflare's
  edge. An application is created with `POST /accounts/{id}/access/apps`
  (`type: self_hosted`, `domain`, `policies`), which needs `Access: Apps and
  Policies Write`; the team domain comes from `GET
  /accounts/{id}/access/organizations` (`auth_domain`), which needs
  `Access: Organizations, Identity Providers, and Groups Read`. Access sends
  the origin a JWT in `Cf-Access-Jwt-Assertion`, signed RS256 with keys at
  `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`, rotated every
  6 weeks with the previous key valid 7 days more. Cloudflare recommends
  validating the header, not the `CF_Authorization` cookie, and checking
  `iss` and `aud` (the application's AUD tag). cloudflared can also verify
  the JWT itself (`originRequest.access: {required, teamName, audTag}`).
  [cf: validating-json], [cf: access apps create], [cf: organizations],
  [cf: origin-parameters]

## Decision

### D1. Two Tailscale ways in for a private address

A private address may be served three ways. Phase 2's is the first; this
ADR adds two, each a value of a new `private_via` column next to the
phase 2 `private_*` columns:

| `private_via` | Address | Needs | Phase |
|---|---|---|---|
| `listener` | your domain, A/AAAA to the private listener | a managed DNS zone | 2 |
| `tailscale_serve` | `<machine>.<tailnet>.ts.net` | Tailscale on the worker, HTTPS on in the tailnet | 3a |
| `tailscale_service` | `<service>.<tailnet>.ts.net` | the above, a tagged worker, a Tailscale OAuth client, host approval | 3b |

`way_in` stays `private:<node>` and `network` stays `Tailscale` for both.
The address row also records `private_service` (the `svc:` name) for a
Service. The range lock of ADR 064 D5 becomes, for both Tailscale ways in,
"arrived through this address's Serve socket" (D3): the TCP peer is
tailscaled, never the visitor.

**How Temps chooses.** The plan offers what fits, in this order, and the
user may pick another:

1. The Service the address already has (a re-run of an earlier setup).
2. **A Service**, when the cluster has a Tailscale credential (D2) and the
   worker reports itself tagged, on tailscaled v1.94 or later. It is the
   default because the name is the app's, not the machine's: it survives
   the app moving to another worker, several apps can each have one on one
   worker, and a second host can be added later (open question 1).
   The suggested name is `<project>-<environment>` (`myapp`, `myapp-staging`),
   editable, a DNS label.
3. **The machine's own name**, when the worker's port 443 on its Tailscale
   address is free: no phase 2 private listener bound there and no Serve
   handler on `<machine>:443` that Temps did not write. This needs no
   credential and no tags, so it is the one-action path for a home lab. The
   plan says what it costs: "`master.example.ts.net` becomes this app's
   address. Another app on `master` needs a Service name."
4. **A Service after pasting a credential**, with the tag and approval
   steps it needs.

A name the user types that is not the machine's name can only be a Service.
The machine name and the phase 2 listener are exclusive on one worker:
Serve on 443 would take the listener's traffic (Context), so the plan
refuses with "`master` already serves private addresses on 100.101.102.103:443;
give this app a Service name instead", and phase 2's plan refuses the
reverse. Services have their own TailVIP and never collide with either.

ADR 064 D5 described the tailnet name as `myapp.<tailnet>.ts.net` through
Serve. Only a Service gives that form; Serve alone gives the machine's name.
This ADR supersedes that sentence.

### D2. The agent drives tailscaled; the control plane drives the Tailscale API

**On the worker: the agent, through the LocalAPI.** Everything that happens
on the machine (Serve config, advertising a Service, status, whois) is the
agent's, over tailscaled's local socket. That needs no credential at all:
access to the socket is the machine's own trust, which the agent already
has. It uses the LocalAPI rather than the CLI because the LocalAPI gives a
conditional write (`If-Match` on `serve-config`, `412` on a lost race),
structured status, and whois, and the CLI's two `serve status` outputs
differ. The cost is that the LocalAPI is not documented as stable, so:

- the agent requires a minimum tailscaled version (1.94, for socket targets)
  and reports the version it found;
- each LocalAPI call the agent makes is covered by a contract test against
  pinned tailscaled releases in CI, and a new release is added before it is
  declared supported;
- an answer the agent does not understand stops the Tailscale way in on
  that worker with "tailscaled 1.x on `master` answered in a way this agent
  does not understand; update Temps or report it". It never guesses.

**Consent is the worker owner's, as for every ingress mode.** Changing
tailscaled's config is a change to the machine, so the agent touches it
only when `agent.json` allows it:

```
temps agent ingress private --listen tailscale --tailscale-serve
temps join … --ingress private --listen tailscale --tailscale-serve
```

`--tailscale-serve` may be given without a bound private listener (it then
serves only through Serve). Without the flag the plan names this command
and the worker, like phase 2's `PRIVATE_COMMAND`. The agent must run as
root, since tailscaled lets only root point Serve at a socket; an agent
that is not root reports so and the plan says it.

**What the agent owns in the Serve config, and nothing else.** Its entries
are the ones whose target is a socket under `/run/temps/serve/`; the
directory is `0700 root`. It reads the config with its `Etag`, changes only
its own entries, writes with `If-Match`, and on `412` re-reads and
re-applies. It never edits, replaces or removes an entry it did not write,
never sets `AllowFunnel`, and never runs `serve reset`. A name or port held
by an entry it did not write is a conflict it reports (D4), not a slot it
takes. It edits `AdvertiseServices` the same way: add or remove only its
own `svc:` names, keep the rest.

**In the tailnet: the control plane, with an OAuth client.** Only Services
need the API (create, read, delete the Service; read its hosts). The
credential is a Tailscale OAuth client registered as an ADR 058 D8 provider,
`tailscale`, with setup fields `client_id` (`text`), `client_secret`
(`secret`) and `tailnet` (`text`, default `-`, the credential's own). The
secret is sealed with `EncryptionService` in `settings.setup_secrets`, never
returned by the API, and its use is audited; nothing is an environment
variable. The control plane exchanges it for short-lived access tokens at
Tailscale's token endpoint and keeps those in memory only.

- **Scopes asked for:** `services` and `devices:core:read`. `services` is
  tailnet-wide: a leaked secret can create, change and delete every
  Service in the tailnet, which the checklist says next to it, as ADR 064
  D8 does for tunnels. `devices:core:read` maps a worker to its Tailscale
  device id and reads its tags.
- **Not asked for: `devices:core`.** It is the scope the API lists for
  approving a host, but it can also authorize and remove devices and change
  their tags, which is far more than a deploy tool should hold. Host
  approval is the tailnet admin's, through the admin console or an
  `autoApprovers` entry the plan shows ready to paste (D4). Whether
  `devices:core:read` is enough to read a Service's hosts is confirmed
  during implementation. If it is not, the Service way in stays unavailable
  until a way to read hosts without `devices:core` is found (D4); Temps does
  not ask for a broader scope. Approval seen locally on the worker (open
  question 2) could end the approval wait, but it cannot replace the host
  read that the Service `done` check and the D5 outside check need.
- **Each scope is verified before anything is created**, the D8 way: a
  list call for read, and for `services` a `PUT` of a deliberately invalid
  Service name, where "forbidden" proves the scope missing and a validation
  error proves it present. The ordering is confirmed against the live API
  during implementation with recorded responses; an answer that cannot
  tell marks the scope unverified and refuses the run.

### D3. The Serve socket: the only place a tailnet identity header is read

For each Tailscale-served address the agent opens one unix socket,
`/run/temps/serve/<address_id>.sock`, and points that address's Serve entry
at it. The socket is the address's listener:

- **The socket names the address, not `Host`.** Serve rewrites `Host` to
  `localhost` for socket targets (Context), so the agent takes the address
  from the socket it accepted on, sets `Host` to that hostname before
  routing (ADR-020, over the mesh to wherever the app runs) and ignores
  `X-Forwarded-Host`. A request cannot name another address.
- **Only tailscaled can reach it.** The socket lives in a `0700 root`
  directory. A container, a connector running with host networking (ADR 058
  D3) or a local non-root process cannot connect. This is why identity is
  read only here and never on a loopback TCP port: anything on the host
  can open `127.0.0.1`, and Serve has no way to add a secret to the request
  that would tell its connections apart.
- **What is trusted on it, and only on it:** `Tailscale-User-Login`,
  `Tailscale-User-Name`, `Tailscale-User-Profile-Pic`, `Tailscale-Funnel-Request`
  and `X-Forwarded-For`. tailscaled deletes client copies of the identity
  headers and sets `X-Forwarded-For` itself (Context).
- **Funnel is refused at the socket.** A request carrying
  `Tailscale-Funnel-Request` is answered `403` with
  `x-temps-private-ingress: funnel`, and the agent reports the address as
  public (D5). The agent also refuses to serve an address whose Serve entry
  has `AllowFunnel`.
- **Everywhere else these headers are removed.** Every other listener (the
  control-plane proxy, direct, tunnel loopback, the phase 2 private
  listener) strips `Tailscale-*` from incoming requests before routing, so
  an app that reads `Tailscale-User-Login` cannot be fed one through a
  public way in.

### D4. Tailscale resources in the plan, the journal and undo

Tailscale steps follow ADR 064 D4 unchanged in principle: plan bound to a
fingerprint, journal before each side effect, authoritative lookups,
conflicts that stop the run, undo that removes only what the run made.

**New step kinds:** `save_tailscale_credential`, `define_tailscale_service`,
`configure_tailscale_serve`, `wait_for_service_approval`, plus the existing
`set_audience` (now carrying `private_via`, `private_service` and, for D6,
the allowed principals) and `check_reachability`. No certificate step:
tailscaled gets the certificate (Context), and nothing is written to
`domains`, as for tunneled domains (ADR 058 D10). The audience is set
before the Serve entry exists, as phase 2 sets it before DNS points
anywhere.

**The fingerprint adds** each candidate worker's Tailscale facts as last
reported (tailscaled version, `BackendState`, tags, MagicDNS and HTTPS on,
`Self.DNSName`, who holds `<machine>:443` in its Serve config, the
`--tailscale-serve` consent, root or not), the credential, the Service as
read from the API when planning (absent, or its `comment`, `ports`,
`tags` and `addrs`), and the address's allowed principals.

| Intent | Found by | Lookup authority |
|---|---|---|
| `TailscaleCredential {name}` | its Temps row | immediate |
| `TailscaleService {name, marker}` | `GET …/services/{name}` and its `comment` | settled (two misses), as Cloudflare DNS |
| `TailscaleServe {node_id, address_id}` | the `tailnet_serve_entries` row and this run's reference to it, then the agent's report | immediate for the row; the step is done only on the report |
| `ServiceApproval {name, node_id}` | none: a wait, no side effect | n/a |

- **Temps' Service definition is fixed, and Temps never updates a
  Service.** Every Service Temps makes has the same definition: `ports`
  `["tcp:443"]`, `tags` the worker tag the plan chose (`tag:temps` by
  default), and `comment: "temps:<install>:<creator_run_id>"`, the ADR 031
  marker idea in the one free field a Service has. Since nothing about it
  ever needs to change, Temps calls `PUT` only to create, never to update.
  That removes the cases where an update could lose someone else's change
  or rewrite the marker.
- **Who owns it, and who created it.**
  - **Ownership is `<install>`** (ADR 031's `install_id`). Planning,
    reconciliation and the outside check treat a Service whose marker
    carries this install as Temps', whatever run created it, so a later
    run never sees this install's own Service as foreign.
  - **The creator run id is written once, at creation, and never
    rewritten.** The journal entry of the run that created it records the
    state `created`. Any other run that finds it (a re-run for the address,
    D1 option 1) records `found`, and it never writes to the Service at
    all.
  - A marker with another install's id, or no marker, is not Temps'. It is
    never written to or deleted.
- **Which hosts are valid: the ones with a Serve entry.** What a worker
  serves for an address is one `tailnet_serve_entries` row per (node,
  address): way in, Service name if any. That row is the only record of a
  worker configuration (D2's Serve entry and, for a Service, its
  `AdvertiseServices` name), and it is shared, not per run (below). The
  valid hosts of a Service are the nodes of its entries, whichever run of
  this install made them; a host advertising the name without an entry is
  foreign.
- **Create only when absent.** The plan reads the name first and records
  what it found in the fingerprint: absent, or the `comment`, `ports`,
  `tags` and `addrs`. A name held without this install's marker blocks the
  option ("`svc:myapp` already exists in your tailnet and Temps did not
  create it; pick another name"). Immediately before the `PUT` the step
  reads the name again:
  - **absent:** create it;
  - **this run's own marker** (a resumed run whose earlier `PUT` landed):
    no `PUT`, go to the check below as created;
  - **this install's marker from an earlier run**, with `comment`, `ports`,
    `tags` and `addrs` all equal to the plan's read: no `PUT`, journal it
    `found`, go to the check below as found. This is the re-run path of D1
    option 1;
  - **anything else** is `conflict`, nothing written, and the message names
    what differs: a foreign marker or none, or this install's Service with
    a field different from the plan's read.
- **The create can still overwrite, and the ADR accepts that window.** The
  Services API has no conditional create (no `If-Match`, no create-only
  verb), so a Service someone else creates between that re-read and the
  `PUT` is replaced by this run's definition and then carries this run's
  marker. The marker alone cannot reveal it, and the pre-`PUT` read, which
  saw nothing, cannot either. Generated names would close the window, but
  the name is the address the user picked (D1), so Temps keeps user-chosen
  names and bounds the damage instead:
  - the window is one request long, the same as for Cloudflare DNS in
    ADR 064 D4, and only an actor who already has a `services`-scoped
    credential for the tailnet can use it;
  - what the overwrite replaces is the other Service's definition (its
    ports, tags and comment). It moves no traffic by itself: a Service
    routes only to hosts that advertise it and that the tailnet admin
    approved;
  - the host check below catches the other actor's devices, and the run
    never approves hosts, so an overwritten name cannot be turned into
    traffic to Temps' worker without the tailnet admin approving it.
- **When the Service step is `done`.** The step reads the whole definition
  and its hosts back:
  - **after creating it:** the marker is this run's, `ports` and `tags` are
    the fixed definition, and `addrs` is non-empty and inside the Tailscale
    ranges. Tailscale assigns the TailVIP, so the run records `addrs` in
    the journal here rather than choosing them;
  - **when it was found:** the marker carries this install (any creator),
    `ports` and `tags` are the fixed definition, and `addrs` equals the
    `addrs` journaled by the run that created it;
  - **in both cases:** every host advertising the name matches a
    `tailnet_serve_entries` row on *both* that host's node and this
    Service's name. A row for the same node that names another Service, or
    none (a machine-name entry), does not make the host valid. The API
    lists hosts as devices without an address, so the address is matched
    on the Temps side: the Service belongs to one address (its
    `private_service`), and the row must be for that address.

  Any mismatch makes the step `conflict`, even with this install's marker,
  and names the field: "`svc:myapp` now listens on `tcp:8443` as well,
  which Temps did not set", or "`svc:myapp` is also advertised by `nas`,
  which no Temps setup configured. Someone may have created the same name
  while Temps did; check it before approving." The reconciler and the
  outside check (D5) keep applying the same rule after the run, so a later
  change to the definition or a new unrecorded host is reported too. It is
  never accepted as the new truth. This check is why reading a Service's
  hosts is required: if implementation finds that the D2 scopes cannot read
  them, the Service way in is not offered until a way to read them without
  `devices:core` is found.
- **The Serve entry is declared, shared by reference, and applied by the
  agent.** One `tailnet_serve_entries` row per (node, address) holds what
  that worker must configure, and the route snapshot carries it to that
  worker only. Runs do not own the row; they reference it. Each run that
  needs it writes a `tailnet_serve_refs` row (entry, run, `created` or
  `found`) in the same transaction. The rows are unique on
  `(node_id, address_id)`. Every change to an entry's references, and the
  last-reference check that deletes the entry, runs in one transaction
  that first takes a transaction-scoped advisory lock keyed on
  `(node_id, address_id)`. The lock is taken even when no row exists yet,
  so a run adding a reference and an undo removing the last one can never
  interleave. A run whose entry names a Service first takes that
  Service's lock (see "Deleting a Service", below), then this one, always
  in that order. Inside that lock:
  - **no row yet:** the run creates the row and a `created` reference;
  - **a matching row** (same way in and Service name), for example a
    re-run for the address on the same worker: the run adds a `found`
    reference and changes nothing;
  - **a row that differs:** the step is `conflict` and names what the
    worker serves now. Two runs never hold different wishes for one slot;
  - **a row being removed** (`removing`, below): the step waits until the
    agent reports the removal, then starts again with no row. It never
    revives a row whose configuration the agent is taking off the worker.

  The agent applies the row with `If-Match` (D2) and reports, per entry,
  `applied` with the config's `Etag`, `conflict` naming what holds the slot
  ("a `tailscale serve` entry proxying to `http://127.0.0.1:3000`"), or
  `error` with the reason. The step is `done` on `applied` and `conflict`
  on `conflict`; it never overwrites. Since tailscaled's write is
  conditional, there is no lost-update window on the worker. On `applied`
  the row records `applied_etag` and that the agent wrote the entry; on
  `conflict` it records that the agent wrote nothing; on `error` it records
  that the outcome is unknown, since the write may have landed before the
  error. The same rule covers the machine-name way in, which has entries
  but no Service.
- **Approval is waited for, never done.** The step shows the admin-console
  link and the policy entry to paste:
  `"autoApprovers": {"services": {"svc:myapp": ["tag:temps"]}}`. Whether
  `autoApprovers` accepts a pattern for all of Temps' Services is not
  documented; until it is, the snippet names each Service. The run waits
  in `waiting` and resumes when the host reads as approved; it is not a
  failure.
- **Undo, newest first.**
  - **References first, worker configuration with the last one.** Undo
    deletes this run's `tailnet_serve_refs` rows under the lock above. An
    entry that still has another run's reference is left exactly as it is,
    and the worker keeps serving. Undo never removes worker configuration
    another run still references. An entry left with no reference is
    handled by what the agent last reported for it:
    - **the agent wrote it** (`applied`, with an `applied_etag`): the row
      becomes `removing`, and the agent removes only that entry and its
      `AdvertiseServices` name, with `If-Match`. It removes only a handler
      whose target is this address's socket, the D2 ownership rule. The row
      is deleted when the agent reports `removed`. If the slot now holds
      something the agent did not write, it removes nothing and reports it.
      The step then becomes `conflict` naming what is there, and the row
      stays until the user decides;
    - **the agent wrote nothing** (it reported `conflict`, which it
      decides from a read before any write): there is nothing of Temps' on
      the worker. The row is deleted without
      any instruction to the agent, so pre-existing configuration is never
      touched;
    - **no report arrived:** the agent may have written the entry and lost
      the report, so silence is not "wrote nothing". The row becomes
      `removing` with no `applied_etag`. The agent reads its current
      config, removes a handler only if its target is this address's
      socket (the D2 ownership rule), and writes with `If-Match` on the
      `Etag` of that read. It drops the `AdvertiseServices` name only if no
      handler for that Service is left. It reports `removed`, or `absent`
      when no such handler was there, and the row is deleted on either.
      Until a report arrives the step stays pending, like any step the
      reconciler is waiting on;
    - **the agent reported `error`:** the write may have reached tailscaled
      before the error (a timeout after the commit, for example), so it is
      handled exactly as no report, above;
    - **the worker is gone from the cluster:** no agent can report, and
      removing a node does not by itself clear its tailscaled. Removing a
      node whose agent still answers first sends these removals through
      the `removing` path above. Otherwise the row becomes `removing` with
      no `applied_etag`, and is deleted only on proof that the machine no
      longer serves it: for a Service, the API's host list no longer
      includes the machine (the D2 host read); for any entry, the machine
      is no longer in the tailnet. Until then the step is `waiting` and
      names the machine: "`nas` left the cluster while it served `myapp`.
      Remove it from the tailnet, or run `tailscale serve reset` on it,
      then confirm." The user may confirm instead, after checking the
      machine, as for a Service deletion. While the row exists, that host
      counts as recorded, not unrecorded, so it blocks deleting the Service
      without reading as someone else's;

    The step is pending until one of these ends it. It is never done just
    because the row was deleted.
  - **Then the Service, only once it has no entries left.** It is deleted by
    the undo that removes its last entry, whichever run of this install
    that is, and only with the user's explicit confirmation (below). Deleting
    it also needs three things:
    - a journal entry of this install recording `created` for it, with the
      run id in its marker;
    - its definition still matches that entry;
    - no unrecorded host advertises it.

    This way neither the creator's undo nor a later run's undo leaves an
    orphan.
  - **Deleting a Service has a window, so a person confirms it.** The
    Services API has no conditional `DELETE` either. The checks above run
    on a read made immediately before the `DELETE`, but someone with the
    `services` scope can still change the Service in between, and the
    delete would remove their change with it. Temps does not close that
    window; it puts a person in front of it:
    - undo never deletes a Service on its own, and the reconciler never
      deletes one at all. The step stops as `waiting` with what it will do:
      "Delete `svc:myapp` from tailnet `example.ts.net`? It was created by
      this Temps install on 3 May and nothing uses it now." It deletes only
      when the user confirms;
    - confirming is a sensitive action (step-up), journaled with the
      definition the user was shown;
    - the confirmed delete runs in one transaction holding a
      transaction-scoped advisory lock keyed on (tailnet, Service name).
      A run takes the same lock before it adds an entry or a reference
      for that Service, so none can be added between the checks and the
      `DELETE`. Under the lock, Temps checks again that no
      `tailnet_serve_entries` row names the Service on any node, so no
      `tailnet_serve_refs` row reaches it, and that no host advertises it.
      It then reads the Service and sends the `DELETE` only if the read
      still equals the definition the user was shown;
    - declining, a read that changed, or a check that no longer holds
      keeps the Service and ends the step as "kept", naming the reason
      ("`svc:myapp` is used again, by `myapp` on `nas`"). The user can
      remove a Service that is truly unused in the admin console. A run
      that waited on the lock and finds the Service deleted creates it
      afresh, as in "Create only when absent".
  - **A Service still in use is kept.** If entries other runs reference remain, undo
    reports "kept: `svc:myapp` still serves `myapp` on `nas` (set up on
    3 May)" and is done. That is expected, not a conflict.
  - **Anything else pauses undo as a conflict:** a changed definition, or an
    unrecorded host ("`svc:myapp` is now also hosted by `nas`, which no Temps
    setup configured. Keep it, or delete it anyway?").
  - **Never deleted:** a Service no journal of this install records as
    `created`.
  - **The rest.** The credential row is removed only if the run created it
    and nothing else uses it. The address's audience goes back last, as in
    phase 2.
- **Reconcile** treats a missing agent report like any pending step: the
  desired row is authoritative for what was asked; the agent's next
  heartbeat says what is true. A worker that does not report for 20
  minutes reads as `unknown`, as phase 2's inside checks do.

### D5. Checks for tailnet names

ADR 064 D7 asks two questions: does it work from inside, and is it closed
from outside. For a tailnet name:

**Inside (the agent, every 5 minutes and after each change):**

1. tailscaled is running (`BackendState` = `Running`) and this version is
   supported;
2. MagicDNS and HTTPS are on and the name is in `CertDomains` (per-machine
   name) or the tailnet suffix is (Service);
3. the address's Serve entry is present with the `Etag` last applied,
   points at its socket, and has no `AllowFunnel`;
4. **the name resolves through MagicDNS to the right place:** the agent asks
   tailscaled's resolver at `100.100.100.100` directly, so the answer does
   not depend on whether the host's own resolver uses MagicDNS, and accepts
   only this worker's Tailscale addresses (per-machine) or the Service's
   `addrs` (Service), in `100.64.0.0/10` or `fd7a:115c:a1e0::/48`. A
   different answer is reported with what it is, as in phase 2;
5. a request over the socket, with the hostname, gets the agent's own
   routing answer for the address (the private-ingress probe, not the
   app), so the socket is bound and routes.

It does not dial the tailnet name from the worker itself: traffic from a
machine to its own Tailscale address does not take the path a visitor's
does, so the result would say nothing about Serve. The console adds
**Check from this device**, which opens the name from the user's browser
and reports whether it loaded, for the end-to-end answer only a tailnet
device can give.

**Outside (the control plane):** no tunnel publishes or is publishing the
name; public DNS returns nothing for it or only Tailscale addresses; and
the agent reports Funnel off for it; and, for a Service, every host
advertising it matches a `tailnet_serve_entries` row on its node and this Service (D4), read
from the API with the D2 credential. A foreign host is shown with its name
and is a failure. A Funnel entry for the name, or a
`Tailscale-Funnel-Request` seen at the socket, is shown in red ("This
private address is also on Tailscale Funnel, which makes it public. Run
`tailscale funnel --https=443 off` on `master`.") and the agent stops
serving it until Funnel is off.

**Failure messages** (D3 of ADR 064: each names what to do):

| Cause | Message |
|---|---|
| no tailscaled socket | "Tailscale is not running on `master` (no `/var/run/tailscale/tailscaled.sock`). Install Tailscale and run `sudo tailscale up`." |
| not logged in | "Tailscale on `master` is `NeedsLogin`. Run `sudo tailscale up`." |
| MagicDNS off | "MagicDNS is off for tailnet `example.ts.net`. Turn it on at login.tailscale.com/admin/dns, then turn on HTTPS certificates." |
| HTTPS off | "HTTPS certificates are off for tailnet `example.ts.net`, so Serve cannot give `master.example.ts.net` a certificate. Turn them on at login.tailscale.com/admin/dns. Machine names become public in certificate logs." |
| old tailscaled | "tailscaled 1.90 on `master` cannot send Serve traffic to a socket. Temps needs 1.94 or later." |
| agent not root | "The Temps agent on `master` runs as `temps`; Tailscale lets only root point Serve at a socket." |
| no consent | "Run `temps agent ingress private --listen tailscale --tailscale-serve` on `master`." |
| host not tagged | "`master` is signed in as alice@example.com, and only tagged devices can host a Tailscale Service. Run `sudo tailscale up --advertise-tags=tag:temps` (the tag must be in your policy's `tagOwners`), or use the machine name `master.example.ts.net`." |
| not approved | "`svc:myapp` is advertised by `master` but not approved. Approve it at login.tailscale.com/admin/services, or add `\"autoApprovers\": {\"services\": {\"svc:myapp\": [\"tag:temps\"]}}` to your tailnet policy." |
| slot taken | "`master.example.ts.net` already serves `http://127.0.0.1:3000` (set with `tailscale serve`, not by Temps). Remove it with `tailscale serve --https=443 off`, or give this app a Service name." |
| Service name taken | "`svc:myapp` already exists in your tailnet and Temps did not create it. Pick another name." |
| credential scope | "This Tailscale OAuth client cannot manage Services. Create one with the `services` and `devices:core:read` scopes." |

### D6. "Only these tailnet users": a new audience, `tailnet_users`

A new audience value, next to `private`. An address with it is private in
every way phase 2 defines (served only by private ways in, never published,
answered 404 by public listeners) and additionally admits only listed
people or devices.

- **`private` never reads identity.** The range-only audience admits by
  where a request came from and nothing else. It never starts trusting a
  header, so an address made `private` in phase 2 cannot become narrower
  or wider because of something a request carries.
- **Where it can be served.** On the Serve socket of a Tailscale way in
  (D3), identity comes from `Tailscale-User-Login`. On the phase 2 private
  listener bound on `tailscale0` (`private_via = listener`), there are no
  Serve headers and none are read: the agent asks the LocalAPI `whois` for
  the TCP peer address, which tailscaled answers from its own view of the
  tailnet. Asking whois is talking to tailscaled, so this too needs the
  worker's `--tailscale-serve` consent (D2). A private listener on a LAN
  range cannot serve `tailnet_users`; the plan refuses it.
- **Who may be listed.** `address_tailnet_principals` rows per address:
  `kind` = `login` (a Tailscale login name, e.g. `alice@example.com` or
  `alice@github`) or `tag` (`tag:ci`). Logins are compared exactly after
  ASCII lower-casing; a Q-encoded header is decoded first and one that
  does not decode is refused. Shared-in users carry their own login and
  are admitted only if listed. One principal is a valid list. An empty list
  is refused by the plan, and a route that arrives with no principals (a
  bug, or rows removed outside a run) admits nobody: it fails closed rather
  than falling back to the whole range. Editing the list is a sensitive action
  (step-up), like changing the audience, and is journaled on the run that
  sets it so undo restores the previous list.
- **Tagged devices carry no user.** Serve sets no identity headers for them
  (Context). A request with no `Tailscale-User-Login` is admitted only if
  the list has a `tag` entry and the LocalAPI `whois` of the
  `X-Forwarded-For` address (trusted on the socket only) returns a node
  with that tag. Otherwise it gets `403` with `x-temps-private-ingress:
  not-allowed` and a page that says why: "This app admits only listed
  tailnet users. Requests from tagged devices (`tag:server`) carry no
  user; ask the owner to add the tag." Whois answers are cached for 30
  seconds per address.
- **Groups are not in phase 3.** Serve's headers carry no groups. The
  Tailscale-native way is an app capability granted in the tailnet policy
  and read from `Tailscale-App-Capabilities` (`--accept-app-caps`, v1.92+),
  which keeps group membership in the tailnet policy where it already
  lives (open question 3).
- **What the app receives.** On the socket the agent passes Serve's
  `Tailscale-User-*` headers through to the app, since apps written for
  Serve expect them, and on both ways in it sets `x-temps-user-login`
  for an admitted identity. Both are stripped from client input everywhere
  else (D3, and `x-temps-*` is already stripped).
- **Agents that cannot enforce it never get it.** The control plane sends a
  `tailnet_users` route only to an agent that reports the
  `tailnet_identity.v1` capability, and holds it back with "update the
  agent on `master`" otherwise, so an older agent can never serve such an
  address to the whole range. For the same reason, `Audience::parse` must
  stop reading an unknown stored value as `private` before phase 2 ships:
  an unknown value becomes a variant no listener serves, so a rolled-back
  control plane cannot widen `tailnet_users` (or `signed_in`) to a whole
  network.

### D7. The signed-in audience: the Temps gate first, Cloudflare Access for tunnels

`audience = signed_in` means: a public way in (ADR 064 D6), plus a gate in
front of the app that admits only people who sign in. The address carries
`gate` = `temps` or `cloudflare_access`.

| Gate | Who can sign in | Ways in | Credential |
|---|---|---|---|
| `temps` (default) | Temps console users with access to the project, optionally only listed teams or users | every Temps listener: control-plane proxy, direct, tunnel | none |
| `cloudflare_access` | whoever the Access policy admits (emails, email domains, the IdPs configured in Access) | tunnels only | a Cloudflare token with Access permissions |

**Recommendation: the Temps gate is the default; Access is offered for
tunneled addresses when the audience is wider than the team.** The
reasoning:

- The question users ask is "only my team". The Temps gate answers it with
  nothing new to configure: the people are the project's members, the
  sign-in is the console's (password and MFA, or the console's OIDC
  providers), and removing someone from the project removes their access
  (D8).
- It works on every way in, including direct ingress and the control-plane
  proxy, and on hosted planes, where Access does not apply and password
  protection does not work today (Context).
- Access is better when the people are not Temps users (clients, a whole
  company domain, an IdP group) and the address is already on Cloudflare.
  It costs an account-wide token (D9) and only fits hostnames Cloudflare's
  edge sees.
- **Password protection is not a signed-in gate.** It is one shared
  secret: no person to revoke, no audit of who came in, a cookie scoped to
  subdomains and a check only the control-plane proxy runs. It stays the
  project setting it is. The plan for a signed-in address on an
  environment with a password says the two both apply.

**The gate runs where the address is served, on every listener.** Unlike
`security` settings, which keep a route on the control plane, the gate is
part of the route snapshot entry, and the agent's ingress enforces it like
the control-plane proxy does. An agent that does not report `gate.v1`
never receives a signed-in route (held back, "update the agent"), so no
listener serves a signed-in address without its gate.

**Paths.** `/_temps/gate/*` on a gated host belongs to the gate (as
`/_temps/password-verify` does to the password wall) and never reaches the
app.

### D8. The Temps gate: sign-in, cookie, logout, revocation

**Sign-in.** A request without a valid gate cookie:

1. gets `302` to the console,
   `https://<console>/_temps/gate/authorize?address=<address_id>&state=<s>&code_challenge=<c>`,
   and a `__Host-temps_gate_state` cookie holding `state` and the PKCE
   verifier (10 minutes, single use). A non-`GET` or non-navigation request
   gets `401` with `x-temps-gate: sign-in` instead, so an API call is not
   redirected into an HTML page;
2. the console requires a full console session (MFA complete where the user
   has MFA), checks that the user may sign in to that address (project
   access through `project_team_access`, and the address's own list of
   teams or users if it has one), and redirects to
   `https://<address>/_temps/gate/callback?code=…&state=…`. **The redirect
   target is derived from the address id on the server**, never taken from
   the request, so the endpoint is not an open redirect;
3. the code is single use, valid 60 seconds, and bound to the address, the
   user's console session and the PKCE challenge. The listener exchanges
   it with the verifier from the state cookie: the control-plane proxy
   directly, an agent over its node-token channel, and an agent may only
   redeem codes for addresses in its own route snapshot. A code seen in a
   log or a `Referer` is useless without the cookie and the node token;
4. the listener sets the gate cookie and redirects to the original path
   (same-origin, sanitised as the password wall already does).

**The cookie.** `__Host-temps_gate=<token>; Secure; HttpOnly; SameSite=Lax;
Path=/`. The `__Host-` prefix makes the browser refuse it unless it is
`Secure`, `Path=/` and has no `Domain`, so it is host-only: never sent to a
sibling or child hostname (the password wall's `Domain=` cookie is). `Lax`,
not `Strict`, so a link from elsewhere arrives signed in. The token is
signed by the control plane's gate key (Ed25519, generated on first use,
private half sealed with `EncryptionService`, public half in the route
snapshot) and holds the session id, user id, address id, issue and expiry
times. The listener checks the signature, that the address id is the
route's own (a token for one app is never valid on another, cookie scope
aside), the expiry, and the revocation data below. It removes the gate
cookies from the request before the app sees it and drops any `Set-Cookie`
from the app that names them. The app receives `x-temps-user` (the opaque
user id) and `x-temps-user-email`, stripped from client input everywhere.

**Lifetime and revocation.** A gate session is a `gate_sessions` row tied
to the console session that created it, and expires at the earlier of 8
hours and that console session's expiry. It is revoked when the user signs
out of the console, is removed from the project or its teams, is disabled,
or when an admin signs everyone out of the address; changing the address's
audience or gate revokes all of its sessions. The route snapshot carries,
per address, a `not_before` time and the ids of revoked sessions that have
not expired yet, so revocation reaches every listener with the next
snapshot, in seconds. A worker cut off from the control plane keeps
honouring tokens until they expire (8 hours at most) and cannot admit
anyone new; the sign-in page says "Sign-in is unavailable: `master`
cannot reach the control plane". That bound is the price of not putting
the control plane in every request's path.

**Logout.** `POST /_temps/gate/logout` on the app host, accepted only with
`Sec-Fetch-Site: same-origin` or a matching `Origin`, clears the cookie and
revokes the session. The console's "Sign out of apps" revokes all of a
user's gate sessions.

**CSRF.** Sign-in CSRF (being signed in as someone else) is stopped by the
`state` cookie and PKCE: a callback without the matching cookie is refused.
The gate's only state-changing endpoint is logout, guarded as above. The
app's own CSRF stays the app's; `SameSite=Lax` keeps the gate cookie off
cross-site `POST`s, so a forged form posts as a signed-out visitor.

### D9. Cloudflare Access for tunneled addresses

- **Plan.** For a signed-in address on a tunnel (ADR 064 D4's tunnel
  options), the plan offers Access next to the Temps gate. It lists the
  account's Access apps first: an app that already covers the hostname is
  offered as "protected by Access app `Grafana`", used as found (Temps
  reads its AUD and never edits or deletes it). Otherwise the plan creates
  one, `self_hosted`, `domain` = the hostname, named
  `temps-<hostname>-<install>`, with a policy from the plan's answers
  (emails, email domains).
- **The token.** A separate Cloudflare token, not the D8 DNS/tunnel token,
  with Account · Access: Apps and Policies · Edit and Account · Access:
  Organizations, Identity Providers, and Groups · Read, stored and checked
  like D8's (sealed, never returned, audited, each permission probed before
  anything is created). Both permissions are account-wide: this token can
  change or remove the protection of every Access app in the account. The
  checklist says so, and that is why it is not merged into the token that
  already reaches every tunnel. Verifying a JWT needs no token: the keys
  are public.
- **Journal.** `AccessApp {name}` is found by the id stored after creation;
  a create that timed out is resolved by listing apps by domain under the
  two-miss rule. Undo deletes only an app this run created whose domain is
  still the hostname, and pauses as a conflict when its policies were
  changed since ("Access app `temps-…` was edited in Cloudflare; keep it or
  delete it?").
- **Verification at the listener, every request.** The control plane
  fetches `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`, keeps
  it fresh (hourly, and on an unknown `kid`, rate-limited), and sends the
  key set, issuer and AUD per address in the route snapshot. The tunnel
  listener requires `Cf-Access-Jwt-Assertion` (the header, never the
  cookie), RS256 only, a known `kid`, `iss` equal to the team domain, `aud`
  containing the address's AUD, `exp`/`nbf` with 60 seconds of skew. A
  failure gets `403` with `x-temps-gate: access`. This holds even if
  someone reaches the tunnel's origin by another route. Where the tunnel
  configuration supports it, Temps also sets cloudflared's
  `originRequest.access` for the hostname, so cloudflared refuses first;
  whether remotely managed tunnels accept it is confirmed during
  implementation.
- **Tunnel only.** An Access-gated address is served only by its tunnel:
  the route table withdraws it from direct listeners and the control-plane
  proxy, and moving it off the tunnel is refused with "This address is
  protected by Cloudflare Access, which only sees traffic through the
  tunnel. Switch it to the Temps sign-in first."

### D10. Checks, health checks, IP allowlists and tunnels for gated addresses

- **Reachability (ADR 064 D7).** For a `temps` gate the control plane
  requests the address without a cookie and expects the gate's own answer
  (`302` to the console's authorize URL with `x-temps-gate: sign-in`); for
  Access it expects Cloudflare's redirect to the team domain. An app answer
  instead is shown in red ("Anyone can open `myapp.example.com`: the
  sign-in is not in front of it") and is a failure, not a pass.
- **Deployment health checks** dial the container, not the address, so the
  gate never affects them.
- **Monitors** (`temps-monitoring` HTTP checks) of a gated address see the
  gate. Temps offers no bypass token or header for its own gate: any
  credential that skips the gate is one more way past it. A monitor of a
  gated address reports "behind sign-in" and offers to check the
  container's health instead. Access service tokens are the user's choice
  in Cloudflare.
- **IP allowlists apply in addition (both must pass).** ADR 064 D9 still
  refuses a private-only allowlist on a tunneled address, signed in or not.
  Environment IP rules still keep every route on the control plane today
  (Context); a signed-in address on such an environment shows that reason
  in its state, as any other address does.
- **Tunnels.** The Temps gate is identity by cookie, not by address, so it
  works through a tunnel unchanged. Access works only through one (D9).

### D11. Self-hosted installs that never opt in are unchanged

Each part acts only when someone opts into it, and the two opt-ins are
independent:

- **Tailscale (D1–D6).** A tailnet name, or `tailnet_users` on any way in,
  needs a user to pick it for an address *and* the worker owner to allow it
  with `--tailscale-serve` (D2). Without both, the agent never opens
  tailscaled's socket and no listener reads a `Tailscale-*` header (they
  are only stripped).
- **Signed in (D7–D10).** `signed_in` needs only a user picking it for an
  address and an agent that reports `gate.v1`. It never needs
  `--tailscale-serve` or Tailscale at all, and works on the direct,
  tunnel and control-plane listeners (D7) of an install that never touches
  Tailscale. `/_temps/gate/*` is reserved only on the hostnames of
  addresses whose audience is `signed_in`; every other host passes those
  paths to the app as today.

Password protection behaves as it does today on every address. Existing
addresses keep their audience and their phase 2 behaviour (below), and the
new columns are empty on every address whose audience is neither `private`
nor `tailnet_users`.

**Migrating phase 2 addresses.** The migration that adds `private_via` (on
`project_custom_domains` and `environment_domains`, next to
`private_range`, `private_node_id` and `private_listener`) backfills
`private_via = 'listener'` on every row whose audience is `private`, since
the phase 2 listener is the only way in such a row can have. It then adds a
check that a `private` or `tailnet_users` row has a non-null `private_via`
and that any other row has none, so no later write can leave a private
address without one. Routing does not default a missing value: a private
row that somehow reads with no `private_via` is served by no listener and
is reported, the same fail-closed rule `Audience::parse` follows (D6).
Because the backfill and the check land in one migration, there is no
window in which a phase 2 address is unserved.

## Security

- A tailnet identity header is read in exactly one place: the per-address
  unix socket in a `0700 root` directory, which tailscaled reaches and no
  container, host-network connector or unprivileged process does (D3). A
  process already root on the worker is outside this boundary, as it is for
  every other secret on the machine. The headers are stripped everywhere
  else. A loopback TCP port is never trusted for
  identity, because every process and host-network container on the
  worker can open one.
- The range-only `private` audience never reads identity (D6), and unknown
  stored audiences serve nobody, so a rollback or an old agent cannot
  widen `tailnet_users` or `signed_in` to everyone on a network or the
  internet. New audiences are sent only to agents that report they enforce
  them.
- Tagged devices are admitted only through an explicit `tag` entry checked
  by whois, never because they lack headers.
- Funnel would make a Serve name public. The agent never sets it, refuses
  traffic marked as Funnel, stops serving an address whose entry has it,
  and the outside check shows it in red (D3, D5).
- The agent edits only Serve entries it wrote, with conditional writes,
  and only with the worker owner's consent. A Service is never adopted by
  name, and Temps never updates one: `PUT` only creates, so neither the
  marker nor anyone else's change is ever rewritten. Ownership is the
  install in its marker. A Service is deleted only when its last recorded
  host is removed and a journal of this install shows Temps created it
  (D4). The Services API cannot create conditionally, so a same-name Service created
  within one request of Temps' `PUT` can be overwritten. Temps detects it
  by the Service's hosts rather than its marker, and never approves hosts
  itself (D4).
- The Tailscale OAuth client gets `services` and `devices:core:read`, never
  `devices:core`; host approval stays with the tailnet admin (D2). The
  `services` scope is tailnet-wide, and the console says so before saving.
- A tailnet name is unreachable from the internet, but not secret: its
  certificate is in public Certificate Transparency logs with the tailnet
  name. ADR 064's "publishes nothing" holds for DNS only; the console says
  this before Create and suggests a Service name that says nothing
  sensitive.
- The gate cookie is host-only (`__Host-`), `Secure`, `HttpOnly`,
  `SameSite=Lax`, signed per address, never visible to the app and never
  settable by it (D8). Sign-in uses state and PKCE; codes are single use,
  60 seconds, bound to the address and redeemable only by the listener
  serving it. The redirect target comes from the server.
- Revocation reaches listeners with the next route snapshot; a worker that
  cannot reach the control plane honours existing gate tokens for at most
  8 hours and admits no one new (D8).
- The Access token is separate from the tunnel token and account-wide; the
  console says so (D9). The listener verifies the Access JWT on every
  request, so reaching the origin some other way does not bypass Access.
- Changing an address's audience, its gate, its tailnet principals or its
  signed-in list is a sensitive action (step-up), as ADR 064 makes audience
  changes.

## Consequences

- A private app gets an HTTPS address with no domain and no DNS provider:
  the machine's tailnet name in one action, or its own Service name.
- "Only these people on my tailnet" and "only my team" become supported,
  checked setups, enforced at the listener that serves the address.
- Signed-in apps work on hosted planes and through tunnels, which password
  protection does not.
- The agent gains a tailscaled client (LocalAPI over its socket) and a gate
  verifier; the control plane gains a Tailscale provider and a gate key.
  Both Tailscale paths depend on behaviour Tailscale does not promise as an
  API, which the contract tests and the minimum version contain.
- Deploying `tailnet_users` and `signed_in` requires agents that report the
  new capabilities; older agents hold those addresses back with a clear
  reason instead of serving them unprotected.

## Phases

| Phase | Scope |
|---|---|
| 2 (prerequisite) | `Audience::parse` fails closed: an unknown stored audience, and `signed_in` before phase 4, is served to nobody, never to a private range (D6). Ships with ADR 064 phase 2, before any later audience can be stored |
| 3a | D1 machine-name way in (`tailscale_serve`); D2 agent LocalAPI client, consent flag, owned entries; D3 per-address socket and header stripping; D4 Serve entry steps and undo; D5 checks and messages |
| 3b | D1 Service way in (`tailscale_service`); D2 Tailscale provider and OAuth client with scope checks; D4 Service, approval wait and undo |
| 3c | D6 `tailnet_users`: principals table, Serve socket identity, whois for tags and for the phase 2 Tailscale listener; capability gating |
| 4a | D7/D8 Temps gate on every listener: sign-in, cookie, logout, revocation, `gate.v1` capability; D10 checks and monitors |
| 4b | D9 Cloudflare Access for tunneled addresses: separate token, app creation or reuse, JWT verification, journal and undo |

## Implementation notes

Where the implementation had to settle something the decisions leave open,
or depart from their letter. Phase 3a:

- **Socket file names.** A Serve socket is
  `/run/temps/serve/<kind>-<id>.sock` (`custom-12.sock`), not
  `<address id>.sock`: the colon of `custom:12` would need quoting in a
  `unix:` proxy target. Ownership is still "a target under
  `/run/temps/serve/`" (D2).
- **`AdvertiseServices` has no `Etag`.** The prefs endpoint takes no
  `If-Match`, so the agent cannot make its update conditional the way the
  Serve config's `Etag`/`If-Match`/412 discipline does. What it does:
  - Every write it makes holds one process-wide lock, so its own writers
    never lose each other's update.
  - It only adds the Service names it serves and removes the ones whose
    last handler it removed. Every other name is kept where it is.
  - It reads the list back after writing. If a name it added is missing,
    another writer replaced the list after this write, so it merges again
    from what the list now says, at most three times, then reports the
    entry as an `error`. A name another writer removed is never put back,
    because the agent cannot tell a lost update from a deliberate removal.

  This does not close the race. If another tool (`tailscale set
  --advertise-services`, say) writes between the agent's read and its
  write, that tool's change is lost, and nothing the agent can read
  afterwards shows that it happened. The window is one LocalAPI round trip.
  The agent's own names come back on the next pass; an operator's change
  lost in that window has to be made again.
- **A changed `Etag` on an entry already applied.** The agent re-verifies
  the handler (still its socket, no `AllowFunnel`) and reports `applied`
  with the new `Etag`, rather than treating any foreign write elsewhere in
  the config as a conflict.
- **Where reports live.** The tailscaled report, entry outcomes and D5
  inside checks travel in one `tailscale` object on the worker's ingress
  report, stored in `node_public_ingress.tailscale` with a `received_at`;
  the inside checks are not a separate table.
- **`tailnet_serve_entries.node_id` has no foreign key**, so a worker
  removed from the cluster leaves its rows for undo to find; undo then asks
  the user to confirm (keep) rather than wait for an agent that will never
  report. Proving the machine left the tailnet needs the API (3b).
- **`service_addrs` is stored on the entry row** (filled from the journal
  in 3b), so the snapshot to the agent needs no extra read.
- **The socket's visitor.** A request on a Serve socket must carry an
  `X-Forwarded-For` that is a Tailscale address (tailscaled always sets
  it); anything else is refused. The D5 step 5 probe is answered by the
  socket only to a request *without* `X-Forwarded-For`, so a tailnet
  visitor can never reach the probe and the agent's own probe never
  reaches the app.
- **Transport failures.** A LocalAPI call that fails in transport (timeout,
  reset) is reported as an error with the state left `ready`; only an
  answer the agent does not understand moves it to `not_understood`.
- **Undo of a Serve entry waits for the agent.** The last reference marks
  the entry `removing` and undo stops "pending" until the agent reports
  `removed` or `absent`; the user undoes again to carry on (the run passes
  through `recovering` then `interrupted` like any pending entry).
- **A conflict with nothing to decide.** A slot held by something Temps did
  not write, or Funnel on the name, stops the run in `conflict` with no
  journal entry in conflict: the worker's owner fixes it on the worker and
  resumes. `resume` accepts exactly that case.
- **A name under the tailnet that is no machine's** is refused in 3a with
  "only a Tailscale Service can give an app any other name there"; 3b turns
  it into the Service option.
- **The way in is still `private:<worker>`** in verdicts and plans, as for
  the listener; the option's title and the verdict's reason say Tailscale
  Serve, and `network` is `Tailscale`.

Open, found while implementing 3a:

- An entry whose runs are all deleted, or whose custom domain is detached
  while another run still references it, is not cleaned up by itself yet.

3b (the Service way in):

- **Reading a Service's hosts.** Tailscale's API spec lists `devices:core`
  for `GET …/services/{name}/devices`, which Temps never asks for. As D2
  and D4 require, Temps does not widen the scopes: the scope check also
  probes that read (a name that exists nowhere, where "not found" proves
  the read allowed and "forbidden" proves it is not), and a probe that
  fails keeps the Service way in unavailable with the credential-scope
  message. Whether `devices:core:read` suffices is decided by the probe on
  each tailnet, not assumed; no live tailnet was available to confirm it.
- **Matching a host to its entry.** The agent now reports tailscaled's
  `Self.ID` (`node_id` in the `tailscale` report). A host matches a Serve
  entry by that id when both sides have one, else by its MagicDNS name.
- **One credential per cluster.** Every Service call uses the cluster's
  single `tailscale` credential row (the oldest if there were several);
  the plan offers pasting one only when there is none. Scope checks are
  cached for 5 minutes per row, host reads for 30 seconds (at most 256
  Services), and access tokens live in memory only.
- **A Service deleted after the define step.** D4 says a run that waited
  on the lock and finds the Service deleted creates it afresh. That holds
  for the define step. A Service deleted *after* it, while the run
  configures Serve or waits for approval, fails the run instead ("`svc:myapp`
  is gone from your tailnet…"), so undo takes the run's pieces apart and
  the user makes the address reachable again. Recreating there would mean a
  second `PUT` from a step that was not planned to write to the tailnet.
- **The Service's own lookup.** A pending create is resolved by reading
  the name and comparing the marker with the run's; a matching marker is
  the run's own (`created`), anything else is not, and nothing is
  written.
- **The outside check** reads the hosts for a Service address on every
  verdict (through the 30-second cache): an unreadable read is `unknown`,
  a host none of the address's Serve entries accounts for fails with D4's
  message, and a worker not approved yet shows the not-approved message.
- **Undo checks the marker, not only the journal.** A Service whose
  comment is no longer the exact marker its creating run gave it (no
  marker, another install's, or another run's) is kept with that reason
  and never offered for deletion, even when this install's journal records
  creating the name: someone may have deleted Temps' Service and made their
  own. The confirmed delete checks the marker again under the (tailnet,
  name) lock, immediately before the `DELETE`.
- **No redirects.** The Tailscale API client never follows a redirect; a
  307/308 would re-send the token exchange's body, `client_secret`
  included. A tailnet name made only of dots is refused.

- **The worker must be in the credential's tailnet.** Before a Service
  run starts, the worker that will host it is looked up through the
  credential by its stable node id (`GET /device/{nodeId}`, within
  `devices:core:read`). Tailscale answers 404 for a device of any other
  tailnet, so this needs no tailnet name: a credential's tailnet `-` means
  the OAuth client's own tailnet, which Temps could not otherwise name.
  Not found, a worker that has not reported its node id yet, or an answer
  Temps cannot get all refuse the run before anything is created.

Open, found in review of 3b:

- The outside check reads a Service's hosts but not its definition; it
  could also compare the definition with the fixed one and check the
  marker, through the same 30-second cache.

3c (only these tailnet users):

- **Where the list lives.** `address_tailnet_principals` holds one row per
  login or tag, keyed by the address id (`custom:<id>`,
  `environment:<id>`) rather than a foreign key to either domain table.
  Rows that no longer parse are dropped when read, so a bad row narrows
  the list and never widens it. Rows of a domain that is deleted are not
  removed yet.
- **Where each header is read.** On a Serve socket the login comes only
  from tailscaled's `Tailscale-User-Login` (Q-decoded), and whois of the
  `X-Forwarded-For` visitor is asked only for its tags, never for a login.
  On the private `tailscale0` listener both come from whois of the TCP
  peer. A request admitted by a tag carries no `x-temps-user-login`.
- **Whois needs consent.** The agent reports `tailnet_identity.v1` whenever
  it runs this version, but asks tailscaled only with `--tailscale-serve`;
  without that flag every `tailnet_users` request is refused (fail closed),
  and the plan refuses a worker without consent up front.
- **Where the capability is kept, and what it decides.** The agent's
  capabilities travel in its ingress report and are stored in
  `node_public_ingress.listening` (`capabilities`, at most 16). That stored
  copy only feeds the plan's "update the agent" refusal. What a snapshot
  carries is decided from the snapshot request alone: the agent names its
  capabilities in `x-temps-agent-capabilities` on every request, because an
  earlier report says nothing about the binary asking now. A worker rolled
  back to an older agent stops sending it, and its next snapshot has no
  `tailnet_users` route.
- **`tailnet_users` routes travel apart.** A stored capability alone could
  not keep the promise above. `tailnet_users` private routes ride in
  `identity_private_routes`, and the routes of their Serve entries in
  `identity_serve_routes`. They are never sent as `private_routes` with a
  list, and their entry in `serve_entries` keeps no route. The agent keeps
  them apart the same way in `routes.json`. An older agent ignores fields it
  does not know, so it serves none of them by construction. That covers the
  wire, and the file it loads after a rollback for the rest of its 300 s
  lease. A test reads both with the 3b agent's types.
- **Who is never admitted.**
  - **The worker's own node**, by its Tailscale addresses or its stable node
    id. Otherwise a process on the worker, or a host-network workload,
    would arrive as the machine's owner on an untagged worker.
  - **Anyone, until the agent has read tailscaled's `status`** and so knows
    which node it is.
  - **A node shared in from another tailnet**, by any tag: whois reports a
    non-zero `Sharer` for such a node, and its tags are named by the other
    tailnet's admins. Its user's login can still be listed. Whether whois
    reports a shared node's tags at all could not be checked without a live
    tailnet; ignoring them is the safe reading either way.
- **Withdrawn consent is not outlived by the cache.** Every cached whois
  answer carries the epoch it was asked in, and a change of LocalAPI access
  starts a new one. A question asked before consent was withdrawn is
  neither used nor cached when its answer arrives after.
- **The console types** for `tailnet_users` and its list were written by
  hand into the generated client, to be regenerated from a running server.

## Open questions

1. Several hosts for one Service: Tailscale routes to any approved host, so
   high availability is a second worker advertising the same name. Is that
   one address with several `private_node_id`s, and how do the inside
   checks combine?
2. Can a host tell from tailscaled alone that its Service is approved (for
   example, the Service's addresses appearing in its netmap), so the
   approval wait works without the API reading hosts? To confirm during
   implementation.
3. Groups for `tailnet_users`: through Tailscale app capabilities
   (`Tailscale-App-Capabilities`), granted in the tailnet policy, or not at
   all?
4. Should a user be able to bring a Service defined by hand in the admin
   console, with no OAuth client? Seeing approval locally (2) is not
   enough: the Service's hosts still have to be readable (D4).
5. Does the Temps gate also admit people who are not console users (an
   invite link, a one-time email code), or is that what Access is for?
6. ADR 064's open question 2 applies here too: do preview environments
   inherit `tailnet_users` and `signed_in`, with the same principals?

## Sources

- [kb/1312] Tailscale Serve: https://tailscale.com/kb/1312/serve
- [kb/1242] `tailscale serve` command: https://tailscale.com/kb/1242/tailscale-serve
- [kb/1552] Tailscale Services: https://tailscale.com/kb/1552/tailscale-services
- [kb/1153] Enabling HTTPS: https://tailscale.com/kb/1153/enabling-https
- [kb/1623] Trust credentials (OAuth scopes): https://tailscale.com/kb/1623/trust-credentials
- [kb/1337] Policy syntax (autogroups, autoApprovers): https://tailscale.com/kb/1337/policy-syntax
- Tailscale OpenAPI spec: https://api.tailscale.com/api/v2?outputOpenApiSchema=true
  (`/tailnet/{tailnet}/services…`, `VIPServiceInfo`, `ServiceHostInfo`)
- [src] tailscale/tailscale at v1.104.2 (and tags v1.92.0, v1.94.0 for the
  socket target): `ipn/ipnlocal/serve.go` (`addTailscaleIdentityHeaders`,
  `addProxyForwardedHeaders`, `proxyHandlerForBackend`),
  `ipn/localapi/serve.go` (`serve-config`, `If-Match`, root for sockets),
  `ipn/localapi/localapi.go` (`whois`), `ipn/serve.go` (`ServeConfig`,
  `AllowFunnel`, `Services`), `ipn/ipnstate/ipnstate.go` (`CertDomains`,
  `MagicDNSEnabled`, `Tags`), `cmd/tailscale/cli/serve_v2.go`
  (`AdvertiseServices`), `wgengine/netstack/netstack.go`
  (`ShouldInterceptTCPPort`)
- [cf: validating-json] https://developers.cloudflare.com/cloudflare-one/identity/authorization-cookie/validating-json/
- [cf: access apps create] https://developers.cloudflare.com/api/resources/zero_trust/subresources/access/subresources/applications/methods/create/
- [cf: organizations] https://developers.cloudflare.com/api/resources/zero_trust/subresources/organizations/methods/list/
- [cf: origin-parameters] https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/cloudflared-parameters/origin-parameters/
- Temps: `crates/temps-proxy/src/handler/password_wall.rs`,
  `crates/temps-entities/src/deployment_config.rs`,
  `crates/temps-routes/src/route_table.rs` (`worker_servable`),
  `crates/temps-routes/src/route_sync.rs` (`worker_route_policy`,
  `worker_private_routes`), `crates/temps-ingress/src/reachability/`,
  `crates/temps-agent/src/private_check.rs`,
  `crates/temps-agent/src/internal_proxy.rs`,
  `crates/temps-entities/src/project_team_access.rs`
