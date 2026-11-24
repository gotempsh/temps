# ADR 058: Ingress modes, edge nodes and a shared app domain

## Status

Proposed — 2026-10-20

Related: ADR-020 (every node accepts ingress for any domain), ADR 031 (managed
DNS records and Cloudflare proxied mode), ADR 080 (ingress is a node
capability, D5–D8), ADR 051 (the callback URL), ADR 088 (workers join with no
public address).

## Context

ADR 080 made ingress a capability of a node: any worker with ingress on takes
public traffic for every app and forwards it over the mesh to the container,
wherever it runs. The pieces exist, but an operator cannot tell from the
console how traffic reaches their apps, and two common setups are missing.

What is confusing today:

1. **Ingress is configured twice.** `nodes.public_ingress_enabled` is a
   toggle in the console; the listener only starts when the agent was also
   started with `--public-ingress-address`, which `temps join` saves into the
   agent config. The toggle can be on while the agent has no address, and the
   operator learns it from `public_ingress_running = false`.
2. **Two things are called "edge".** A worker with ingress on is the edge of
   the cluster, while the `temps-edge` crate is a separate single-origin
   cache with its own registration (ADR 048 lists it as broken and to be
   folded into the ingress engine).
3. **Two things are called "public ingress".** The worker listener for app
   traffic and ADR 051's `public_ingress_url`, the URL third parties call
   back on, share a name and live on different pages.
4. **Roles and ingress use different mechanisms.** `temps.sh/role=builder`
   and `=dedicated` are labels; ingress is a column. A dedicated edge
   (ingress on, no workloads) can only be expressed by combining both.
5. **Nothing shows the way in.** The preview domain is a setting, DNS is on
   the Domains page, ingress is per node. On a hosted control plane the
   preview domain defaults to `localho.st`, so a deployed app has a URL that
   resolves to the visitor's own machine.

What is missing:

- **TLS terminated before the worker.** Behind a load balancer, a reverse
  proxy the operator already runs, or Cloudflare, the ingress must serve plain
  HTTP and trust the forwarded client address from that upstream only.
- **Workers with no public address taking traffic.** ADR 055 lets such a
  worker join; nothing lets it serve. Tunnels are the common answer, and
  there are many: Cloudflare Tunnel, ngrok, Tailscale Funnel, frp, Pangolin,
  a WireGuard link to a VPS the operator owns. Temps must not pick one.
- **One domain across many ingress nodes**, kept in sync as nodes come and go.

## Decision

### D1. One kind of node; ingress is a capability with a mode

A node is a worker. "Edge" is not a node kind: it is a worker with ingress
on. A **dedicated edge** is a worker with ingress on and workloads off; the
console shows it with an **Edge** badge derived from those two facts, not a
third label.

Ingress becomes one per-node setting, stored on the node row and read by the
agent from the control plane (no new environment variable, no agent flag):

| Mode | Listens on | TLS | Client address from |
|---|---|---|---|
| `off` | — | — | — |
| `direct` | the node's public interface, :80/:443 | the ingress (HTTP-01 relayed via the control plane, or a cluster wildcard) | the TCP peer |
| `behind_proxy` | an operator-chosen address, plain HTTP | the upstream | `X-Forwarded-For`/`-Proto` or PROXY protocol, only from `trusted_cidrs` |
| `tunnel` | loopback, plain HTTP | the tunnel provider | the provider's client-address header, only from the local connector |

Columns: `ingress_mode` (replacing `public_ingress_enabled`, migrated as
`true → direct`), `ingress_listen_address`, `ingress_trusted_cidrs`, and for
`tunnel` an `ingress_connector_id` naming a connector (D3). The
existing `public_ingress_*` status columns stay, renamed to `ingress_*`.
`--public-ingress-address` keeps working as an override and a deprecation
warning, so no existing worker changes behaviour.

Workloads off uses the existing role label: `temps.sh/role=edge`, which the
scheduler skips like `builder` (ADR 048 planned this exclusion; phase 4
checks it is in place and adds it if not).

### D2. Ingress modes for a shared domain

All modes serve every app host from the route snapshot (ADR-020), so the
app domain does not follow deployments. The setups an operator chooses from,
each a combination of node modes and DNS:

1. **DNS to each ingress** — `direct` nodes, one A record per node. Simple;
   failover only as fast as DNS, so Temps keeps the record set to healthy
   nodes (D4) with a short TTL.
2. **Load balancer or floating IP** — `direct` (TCP passthrough) or
   `behind_proxy` nodes behind the operator's LB; one A record. Health checks
   at the LB. The recommended setup when nodes have public addresses.
3. **Cloudflare proxied** — `direct` with a Cloudflare Origin CA certificate,
   or `behind_proxy` with Cloudflare's ranges as `trusted_cidrs`; proxied
   records per node (ADR 031), or a Cloudflare Load Balancer pool.
4. **Tunnel** — `tunnel` nodes all run the same connector (D3), so the
   provider sees several replicas of one tunnel; one DNS record points the
   domain at the provider (for Cloudflare, a wildcard CNAME to
   `<tunnel-id>.cfargotunnel.com`). The provider routes to any live replica,
   so adding a node adds capacity with no DNS change. The recommended setup
   for nodes with no public address, and the only one that needs no inbound
   port anywhere.
5. **Mixed** — tunnel replicas and proxied `direct` nodes behind one zone.

Cloudflare's free certificate covers one subdomain level: `*.apps.example.com`
needs Advanced Certificate Manager, or ADR 031's flat hostnames
(`app-apps.example.com`). The console says which applies before the operator
picks a domain.

### D3. Tunnels are connectors, defined as data

Temps knows nothing about any tunnel product. In `tunnel` mode the ingress
listens on loopback and serves plain HTTP; something else carries traffic
from the provider to it. That something is a **connector**: a container the
agent runs next to the ingress, described by a definition the operator can
write, edit or import, the same way for every provider.

A connector definition holds:

| Field | Example (Cloudflare) | Example (ngrok) |
|---|---|---|
| `image`, pinned by digest | `cloudflare/cloudflared@sha256:…` | `ngrok/ngrok@sha256:…` |
| `args`, templated | `tunnel --no-autoupdate run --url {{ingress_url}}` | `http {{ingress_url}} --url {{hostname}}` |
| `secrets`: named fields, stored encrypted | `TUNNEL_TOKEN` | `NGROK_AUTHTOKEN` |
| `client_address_header` | `CF-Connecting-IP` | `X-Forwarded-For` |
| `dns_target`, optional template | `{{tunnel_id}}.cfargotunnel.com` | — |
| `health`, optional | its metrics endpoint | its API |

- **Templating** is limited to values Temps owns (`ingress_url`, the node
  name, the app domain) and the definition's own non-secret fields. Secrets
  only reach the container as environment variables, never in `args`, so
  they never appear in process lists or logs.
- **Placement** is per cluster: the definition lives in the control plane
  (a `ingress_connectors` table, secrets via `EncryptionService`) and each
  `tunnel` node references one. Every node with the same connector runs a
  replica of the same tunnel, which is what gives setup 4 its failover.
- **Lifecycle** is the agent's: it pulls the image by digest, runs it with
  `--network host` restricted by the node firewall to the loopback listener
  and outbound traffic, restarts it with the same budget as other children,
  and reports `running`, `restarts` and the last error in the node's ingress
  status. Changing a definition rolls it out to the nodes that use it.
- **Built-in presets** ship as data for the common providers (Cloudflare
  Tunnel, ngrok, Tailscale Funnel, frp client). They are definitions like any
  other: the operator can copy and change them, and Temps has no code path
  that checks which one is in use. A provider nobody has heard of works the
  same way: give an image, its arguments and the header it puts the client
  address in.
- **DNS** is optional and also provider-agnostic: when a definition has a
  `dns_target` and the cluster has a DNS provider (ADR 031), Temps creates the
  record for the app domain pointing at it, with the ownership marker, and
  never overwrites a record it did not create. Otherwise the console shows
  the record to add.
- **Without a container runtime** (a node that runs Temps but not Docker),
  the definition may name a binary instead of an image; the agent supervises
  it the same way. Installing that binary is the operator's job.
- Creating the tunnel at the provider stays with the operator: they paste
  the provider's token once. Temps calling provider APIs to create tunnels
  would need account-wide credentials per provider and is left out.

A connector is also how a **self-hosted edge** works: a definition that runs
a WireGuard or frp client dialing a small VPS the operator owns, which
forwards :443 down the link. No third party is involved.

ADR 048 rejected tunnels as the way to reach the control plane, because that
ties every install to a third party. As an opt-in app ingress mode that
objection does not apply: nothing else depends on it.

### D4. The way in is visible and kept in sync

- **Cluster ingress summary** at the top of the node table: the app domain,
  where it points and its health, e.g. "`*.apps.example.com` → 3 ingress
  nodes (2 healthy)" or "→ tunnel `prod`, 2 connectors". An **Ingress**
  column next to Mesh shows each node's mode and state.
- **Health-driven DNS** for setups 1 and 3 when a DNS provider is
  configured: the A/AAAA set follows healthy ingress nodes, with a floor of
  one record so a control-plane outage never empties it.
- **Add a worker node** gains an optional step 4, *Take app traffic on this
  machine?*, offering the modes that fit what the control plane knows about
  the node (no public address → tunnel or none).
- The preview domain is part of this summary. A cluster whose preview domain
  resolves only locally says so next to every app URL it produces, instead of
  handing out a link that cannot work.

### D5. Names

- "Ingress" means app traffic into the cluster, everywhere in the console and
  API.
- ADR 051's `public_ingress_url` is presented as the **Callback URL**; the
  setting key is aliased, not renamed, so existing installs keep it.
- `temps-edge` is described as an experimental cache until ADR 048 folds it
  into the ingress engine; the console never calls it an edge node.

### D6. Locality

An ingress forwards to a replica on its own node when one is healthy, and
over the mesh otherwise. This removes a network hop for every request that
lands on a node already running the app, which matters most for setup 4,
where Cloudflare picks the connector.

### D7. Direct ingress behind NAT: listen, advertise, verify

The networks workers sit on vary without end: a VPS with the public IP on
its interface, a home server behind a router, a cloud VM behind a 1:1 NAT,
a box behind carrier-grade NAT. `direct` mode treats three things as
separate, because NAT is exactly what makes them differ:

| | Meaning | Default |
|---|---|---|
| **Listen address** | the local interface address the agent binds | automatic: the address of the interface holding the default route |
| **Local ports** | the ports bound on it | 80 and 443; others when the router forwards 80→8080 |
| **Public address** | what DNS points at, and what the check dials | the listen address when it is publicly routable, otherwise asked for |

Public ports are always 80 and 443: browsers and HTTP-01 assume them, so a
public non-standard port is not a website.

- **The worker decides whether it is on the internet.** Opening ports on a
  machine is the machine owner's call, not something a control plane can
  grant itself, so the worker carries an ingress mode in `agent.json`:

  | Mode | Who picks the listen address and ports | Set with |
  |---|---|---|
  | `off` (default) | nobody: no public ports | `temps agent ingress off` |
  | `fixed` | the worker | `temps agent ingress fixed --listen auto\|IP [--http-port] [--https-port]` |
  | `delegated` | the console, among the worker's own addresses | `temps agent ingress delegated` |

  `temps join --ingress …` sets it at enrollment, and Add worker asks "Can
  this worker take public traffic?" to generate that flag. A rejoin keeps
  the saved mode. The running agent re-reads `agent.json` every few
  seconds, so a change needs no restart. A legacy
  `--public-ingress-address` means `fixed` on that address; a worker
  without either is `off`, so upgrading opens nothing.
- **The console routes, the worker binds.** Turning ingress on in the
  console makes the control plane send traffic and DNS to the worker; it
  never opens a port. For a `delegated` worker the listen address and ports
  live in a `node_public_ingress` row and reach the agent in the route
  snapshot, which it binds, re-binds on change and unbinds when turned off,
  with no restart. The agent reports its mode; the control plane refuses
  listen changes for `off` and `fixed` workers and refuses turning an `off`
  worker on, each answer naming the command to run on the worker. Agents
  that report no mode predate this and keep their flag. The public address
  and the reachability check are the control plane's in every mode.
- **The agent reports what it can bind.** Each heartbeat carries the host's
  addresses (Temps' own and container bridges left out), marking the one on
  the default route, plus what is actually bound and why not. The console
  offers those instead of asking the operator to type an interface IP.
- **No rejoin for the ingress key.** A worker enrolled before worker
  ingress has no key for certificate bundles; the agent generates one and
  registers its public half with its node token.
- **The check says what is wrong.** "Check reachability" makes the control
  plane dial the public address on :80 with a fresh nonce, which the node
  answers with a SHA-256 of a fixed label, its token hash and the nonce, then
  open :443. The outcomes are specific:
  refused (a machine answered but nothing listens: the forward is missing),
  timed out (a firewall drops it), wrong answer (the forward points at
  another machine), unroutable (a private or CGNAT address cannot be reached
  from the internet: use a tunnel). It runs from the control plane, so the
  result says so when that is inside the same network and hairpin NAT may
  not apply.

### D8. Ingress providers are registered and describe themselves

Every way into a worker after the first is a new provider: ngrok, Tailscale
Funnel, frp, a WireGuard link to a VPS the operator owns. Each must cost one
backend file, and no console code.

- **A provider describes itself.** `GET /ingress/providers` lists, per
  provider: its name, a one-line summary, what the operator needs ("a domain
  on Cloudflare"), whether it needs a public IP or works behind carrier-grade
  NAT, who handles DNS (Temps, the operator, or the provider's own hostname),
  and its **setup fields**. A field has a key, label, help text, kind and
  whether it is required. Kinds are `text`, `secret`, `dns_provider` (a
  picker of the cluster's DNS providers, optionally of one type) and
  `managed_domain` (a picker of the domains the chosen DNS provider manages).
  The console renders the setup form, the recommendation and the status from
  this, so it has no code per provider.
- **A registry, not a `match`.** A provider registers a factory with
  `info()`, `validate(answers)` and `build(connector)`; the last returns the
  `TunnelProvider` that does the work (create the tunnel, the node spec,
  publish, health, delete). The service never names a provider.
- **One create call for all.** `POST /ingress/connectors` takes `{name,
  provider, values, node_ids}`, `values` keyed by the provider's field keys.
  The service checks what every provider shares (required fields, that the
  DNS provider exists and manages the domain) and the factory checks the
  rest. `dns_provider` and `managed_domain` answers go to their columns,
  `text` answers to `settings.setup`, and `secret` answers to
  `settings.setup_secrets`, sealed as one value with `EncryptionService`.
  The API never returns them. The rest of `settings` belongs to the provider,
  which rewrites it on every `ensure`; the service carries the two setup keys
  across.
- **A tunnel may sit on a subdomain.** A `managed_domain` field with
  `allow_subdomain` takes the zone or a name under it (`lab.example.com`).
  The tunnel then publishes that name and the hostnames under it, never the
  rest of the zone; record names stay relative to the zone. On the zone
  itself, hostnames are kept one level deep, where Cloudflare's free
  certificate reaches. On a subdomain they may go deeper, and the console
  says before creation that `*.lab.example.com` needs Advanced Certificate
  Manager or Total TLS.
- **One status shape.** Every way in reports state, health and the last error
  phrased as what to do, so the console shows them the same way.
- **One consent rule.** A worker runs a provider's connector only when it
  delegated public ingress (D7). The agent enforces it for every provider,
  and the control plane refuses to assign a connector to an `off` or `fixed`
  worker with the command that changes it. Neither check names a provider.
- **The worker card says how visitors reach it.** One section lists the ways
  in: direct (D7) and each registered provider, recommended from what the
  worker reported (a public interface address: direct; a private one: direct
  with a port forward, or a tunnel; carrier-grade NAT: a tunnel).

Cloudflare Tunnel is the first provider on this path. A provider whose
connector is "a program with a token" (most of them) needs only its
`TunnelProvider` and an artifact list; the agent's process driver runs it.

### D9. The operator decides what each tunnel publishes

A tunnel publishes a hostname by pointing a proxied record at itself. Until
now it took every worker-served hostname under its domain, and when two
tunnels covered the same one, whichever reconciled first kept it. That is
the right default and the wrong only option.

- **Per tunnel: everything, or only what is chosen.** `auto_publish` (default
  on) publishes every worker-served hostname under the tunnel's domain; off,
  it publishes only hostnames chosen for it.
- **Per hostname: automatic, a tunnel, or none.** `ingress_hostname_choices`
  holds an operator's choice for one hostname: a tunnel, or no tunnel (the
  operator points its DNS somewhere else). No row means automatic.
- **One deterministic rule.** A choice wins. Otherwise the most specific
  automatic tunnel whose domain covers the hostname, the oldest on a tie. A
  hostname moving between tunnels has its record replaced in place, never
  removed first, so it does not go dark in between.
- **Only what workers serve.** The tunnel and the worker route snapshot use
  one function for "served by workers" (`worker_public_routes`), so a tunnel
  never publishes a hostname its workers would answer with a 404: one held
  on the control plane by a policy, a redirect, wake-up, attack mode, path
  routing, or a backend that is not on a worker.
- **A tunnel in trouble keeps its records.** If the tunnel is not ready or
  its domain is no longer managed, its existing records stay: a provider
  outage must not unpublish everything it carries.
- **Every hostname says why.** `GET /ingress/hostnames` returns, per
  hostname, the tunnel that publishes it (or would), the state, and when it
  is not published the reason and what to do. The console shows it on the
  worker's tunnel card and next to each project domain, with the choice
  beside it.

Not yet: the zone apex, and hostnames that are not served by workers falling
back to a record that points at the control plane.

### D10. A domain can be added as tunneled

The Domains page only knew certificates: adding a domain meant choosing an
ACME challenge, and a project could only attach hostnames from that list.
A tunneled hostname needs no certificate (the provider terminates TLS at its
edge), and issuing one is harmful: the proxy redirects plain HTTP to HTTPS
for hosts with a certificate, and tunnel traffic arrives as plain HTTP.

- **A third way to add a domain.** When a tunnel's domain covers the name,
  the Add Domain wizard offers "through the tunnel" next to HTTP-01 and
  DNS-01. An exact name records a D9 choice for that tunnel; a wildcard
  (`*.example.com`) turns on the tunnel's `auto_publish`, which is what
  "every name under it" already means. Nothing is written to `domains`, so
  certificate issuance, renewal and the proxy's certificate loading are
  untouched. The wizard can attach the name to an environment in the same
  step.
- **One list of domains.** The Domains page is a single table: certificate
  domains and tunneled ones (each tunnel's `*.domain` plus every name routed
  through one), with a column saying how each gets HTTPS. Tunnel rows open a
  page of their own (`/domains/tunnel/{name}`): state, why it is not live,
  its route, and for `*.domain` the workers and links. They are not rows in
  the `domains` table, which stays a certificate store. The project's domain
  picker offers each tunnel's `*.domain` (pick it, type the subdomain) and
  the names chosen for a tunnel, so attaching one never needs a certificate.
- **Where a user starts: importing the zone.** Importing a zone from a DNS
  provider that also runs tunnels (Cloudflare) asks how visitors reach apps
  under it: DNS pointed at a server, or a tunnel on chosen workers. The tunnel
  is created right after the zone; if that fails the zone stays and the
  message says where to retry.
- **API and agents first.** Every step is an `/ingress/*` operation with a
  description an agent can act on. The AI tools read all of them and may
  propose the reversible writes (route a hostname, change what a tunnel
  publishes, move or re-sync it, create one for a managed zone); deleting a
  tunnel stays with a human. The CLI has a `tunnels` group, and the
  custom-domain skill checks for a covering tunnel before reaching for a
  certificate.
- **The provider's certificate has limits, and the console says so.**
  Cloudflare's Universal SSL covers the zone and one level below it; a
  deeper name is flagged in the wizard rather than discovered as a TLS error.

## Security

- `behind_proxy` trusts forwarded headers only from `trusted_cidrs`; an
  empty list is refused. Without this, any client could choose the address
  IP rules and rate limits see.
- `tunnel` trusts the connector's `client_address_header` only on the
  loopback listener; the listener never binds a routable address in this
  mode, so nothing but the local connector can set that header.
- Connector secrets are credentials for the operator's provider account.
  They are stored encrypted, sent only to agents of nodes that use the
  connector, passed to the container as environment variables, never returned
  by the API after they are saved, and their use is audited.
- A node that stops running a tunnel still holds its secret, which would let
  it keep receiving the tunnel's traffic from anywhere. So the connector
  records every node handed the current secret, before it is handed (on
  assignment and again when the node fetches its spec); when one of them is
  removed from the connector or deleted from the cluster, the next reconcile
  (within a minute) rotates the secret at the provider (Cloudflare: a new
  `tunnel_secret`). The new secret changes the spec revision, so the nodes
  that stay restart with it, and open connections no staying node reports as
  its own are cut. When a running node has not reported which connection is
  its own, every connection is cut and the staying nodes reconnect with the
  new secret. The departed nodes stop counting as holders only once the
  secret is rotated and their connections are cut; a failed rotation puts
  the connector in error, naming the nodes that still hold a working secret,
  and is retried every minute.
- A tunnel deleted at the provider is created again on sync. Every hostname
  it carried is then marked stale and published again through the new
  tunnel, so no DNS record keeps pointing at the one that is gone.
- A tunnel is reused only by the id this connector stored, never adopted by
  name: two installs sharing a provider account and picking the same
  connector name would otherwise become replicas of one tunnel, one
  overwriting the other's routing and taking half its traffic. New tunnels
  are named `temps-<name>-<install>` (the first eight characters of the
  install's id); a name already taken by a tunnel this connector does not own
  is refused with what to do.
- Only active workers can be assigned a connector.
- A connector runs an operator-chosen image on the node with the node's
  trust. Images are pinned by digest; editing a definition is a sensitive
  action (step-up), like changing a node's ingress mode, because it decides
  what code receives the cluster's public traffic.
- Changing a node's ingress mode is a sensitive action (step-up), like
  removing a node: it moves where public traffic is terminated.
- The reachability answer on :80 reveals nothing to anyone else: it is a
  digest of the caller's nonce keyed on the node's token hash, which only the
  node and the control plane hold. Nonces are fixed-length and chosen by the
  control plane, so the answer to one cannot be extended into another. Registering an ingress key needs the node
  token, the same credential that already receives certificate bundles.
- The check dials only the address the operator saved for that node, and
  refuses loopback, link-local and private ranges, so it cannot be pointed
  at the control plane's own network.
- Tenant wildcard certificates go only to that tenant's ingress nodes
  (ADR 048 D8).

## Consequences

- An operator sees in one place how traffic reaches their apps and can put
  workers with no public address behind any tunnel provider, or their own
  VPS, without opening a port. Temps carries no provider-specific code.
- Existing workers keep serving: `public_ingress_enabled = true` becomes
  `direct`, and `--public-ingress-address` still works.
- The agent gains one generic capability, supervising a connector container
  or binary. Supporting a new provider is a definition, not a release.
- Temps Cloud can offer the same summary for hosted control planes, whose
  proxy is off (ADR 048 D5): the managed `ingress.<tenant>` name of D8 is the
  Cloud-run equivalent of setup 1, and a metered relay for tenants with only
  NAT'd workers is the Cloud-run equivalent of setup 4.

## Phases

| Phase | Scope |
|---|---|
| 0 | D7: direct ingress configured in the console, applied live, with the reachability check |
| 0 | D8: provider registry and descriptors, generic connector API and console; Cloudflare Tunnel as the first provider |
| 1 | `ingress_mode` column and migration; agent reads the mode from the control plane; `behind_proxy` with trusted CIDRs; Ingress column and cluster summary; names (D5) |
| 2 | `tunnel` mode, connector definitions and their supervision, presets for Cloudflare Tunnel and ngrok, DNS through `dns_target`; step 4 of *Add a worker node* |
| 3 | Health-driven DNS for setups 1 and 3; locality preference |
| 4 | Edge badge and `temps.sh/role=edge` scheduling exclusion in the console |

## Open questions

1. Should `behind_proxy` support the PROXY protocol in phase 1, or only
   forwarded headers? Load balancers in TCP mode need the former.
2. Can presets be published and versioned outside a Temps release (a
   catalog the control plane fetches, like plugin listings), or do they ship
   in the binary and get copied into the database on first use?
3. Should a connector be able to run on a node that is not an ingress, as a
   plain sidecar, or is ingress its only use?
