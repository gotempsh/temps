# ADR 066: Shield: reputation, fingerprints and challenges at every ingress

## Status

Proposed — 2026-10-25. Phase 1 (the challenge fix) shipped with this ADR.
Phase 2 (reputation lists in `log` mode) shipped on 2026-10-26, with a
correction to what D2 said about the lists' terms. Phase 3 (JA4) is being
implemented in the order its entry in Phases gives: the parser, the
control plane for direct, Cloudflare and Bunny clients, the local proxy
header and workers are done; the later phases are proposed. File and line references are to
`main` at 41dcd5c, before phase 1, and to pingora-core 0.9.0 and the vendored
OpenSSL 3.6.2 it builds with.

Related: ADR-020 (every ingress serves every app), ADR 048 (ingress is a
node capability), ADR 051 (public ingress surface; phase 1 moves the CAPTCHA
verify route it lists from the console into the proxy), ADR 055 (workers
with no public address), ADR 058 (ingress modes, Cloudflare Tunnel), ADR 064
(making a project reachable).

## Context

Every app on temps is reachable straight from the internet, and the
internet probes it all day. On one temps host, the first day of netwatch
(our eBPF host monitor) showed:

- requests for `/.env` on the raw server address and for `/.git/HEAD` on a
  hosted app's domain, looking for leaked secrets;
- both from networks on Spamhaus DROP, a list of 1,593 ranges run by or
  hijacked for cybercrime, whose maintainers advise dropping all their traffic;
- scanners from other listed ranges reaching ports 80 and 443 served by temps.

netwatch sees these at the host, but it cannot read HTTPS and it can only
drop packets. The proxy decrypts every request and can answer with a page,
so HTTP-level decisions belong in temps.

### What exists

| Part | Where | Today |
|---|---|---|
| Proof-of-work captcha | `temps-captcha-wasm` (`solve_challenge(challenge, difficulty, cb)`), `temps-proxy/src/handler/captcha.rs`, `temps-proxy/captcha/challenge.html` | SHA-256 of `challenge ‖ nonce`, 20 leading zero bits, solved in WASM with a JS fallback; on success the page reloads |
| Attack mode | `projects.attack_mode`, `environments.attack_mode` (NULL inherits), `proxy.rs:6381–6502`, `service/challenge_service.rs` | Challenges every visitor; 426 without TLS; completions in `challenge_sessions` (24 h) behind a moka cache |
| Global IP blocklist | `service/ip_access_control_service.rs` | `ArcSwap<Vec<IpNetwork>>`, refreshed every 30 s, linear scan in `early_request_filter` (`proxy.rs:4983`) |
| Project IP rules | `ip_access/gate.rs`, `ip_access/cidr_set.rs`, trait `temps_core::ProjectIpGate` | Merged integer ranges, `partition_point` lookup, ≤256 CIDRs per policy, each proxy polls a Postgres sequence every 5 s |
| Rate limiter | `rate_limiter.rs` | Fixed 1-minute and 1-hour windows per (scope, IP), in memory, per process |
| Bot detection | `crawler_detector.rs`, `ai_agent_detector.rs` | User-Agent regex only, used for analytics after the response |
| "TLS fingerprint" | `tls_fingerprint.rs` | SHA-256 of `version-cipher-IP-UserAgent`, shown as `t13_<hex>`. It is not JA4: it reads no ClientHello, so it is effectively a hash of IP and User-Agent |
| Request logs | `service/proxy_log_batch_writer.rs`, ClickHouse `proxy_logs` | Batched off the hot path, 30-day TTL; ClickHouse optional (TimescaleDB otherwise) |
| Worker ingress | `temps-agent/src/internal_proxy.rs`, `public_ingress.rs` | A separate axum/hyper/rustls proxy, no database, fed by the route-sync long poll |

### Problems

1. **Attack mode can be bypassed with no work.** `verify_challenge`
   (`captcha.rs:74–119`) checks only that `sha256(challenge ‖ nonce)` has 20
   leading zero bits, then calls `mark_challenge_completed` with
   `environment_id`, `identifier` and `identifier_type` taken from the
   request body. The challenge is generated in `generate_challenge_html`
   (`proxy.rs:1779`) and stored nowhere. Anyone can pick a challenge,
   precompute one nonce, and mark any identifier as verified for any
   environment for 24 hours. The page even prints the caller's identifier.
2. **All or nothing.** Attack mode challenges every visitor, so it is used
   only as an emergency switch. Nothing reputation-based exists.
3. **Security pins apps to the control plane.** `worker_public_routes()`
   (`temps-routes/src/route_sync.rs:501–560`) keeps every public route off
   workers when global rate limits, security headers, any IP rule or a
   request policy exists, and `snapshot_worker_public_routes`
   (`route_table.rs:3463–3495`) excludes any project with attack mode or a
   security config. A protected app cannot be served from the node it runs on.
4. **No shared learning, no reasons.** A scanner blocked on one ingress is
   unknown to the others, and denials are counted only as atomics
   (`metrics.rs:121–125`).
5. **Smaller defects.** `challenge_sessions` is never pruned
   (`clear_expired_sessions` has no caller); placeholders in
   `challenge.html` are substituted without HTML escaping
   (`proxy.rs:1800–1807`); the verify endpoint and the hot path use separate
   `ChallengeService` caches, so a solved visitor waits up to 5 s.

### Lessons taken from PoW Shield

[PoW Shield](https://github.com/RuiSiang/PoW-Shield) (MIT, Node) is a proof-of-work proxy with
a rate limiter, an IP blacklist, a regex WAF and a controller (PoW Phalanx)
that syncs instances. It does five things we adopt:

- the issued prefix lives in a signed session and the nonce carries a
  timestamp checked against a validity window, so precomputed answers fail;
- clearance is revoked after a number of requests (100 per session per hour
  by default), so a bot pays per batch of requests, not once a day;
- the controller raises difficulty for every instance under attack;
- "whitelist tokens" let API clients and monitors skip the challenge;
- it records solve times to tune difficulty.

And two it warns against: its generic regex WAF ships with five rules
disabled for headers by default (`14,33,80,96,100`) to avoid false positives,
and its default difficulty (13 bits, about 5 s in JavaScript) costs a native
solver about a millisecond. Proof-of-work slows bots; it does not
authenticate anyone.

### Facts this decision relies on

- The proxy runs on the control plane and, as a separate implementation, on
  every worker with public ingress (ADR 048, 051). Workers have no database
  and receive everything over the route-sync long poll
  (`GET /api/internal/nodes/{id}/routes/snapshot`, held 25 s, bearer token
  per node, `route_sync.rs:260–333`).
- Secrets already travel to workers inside that snapshot: TLS keys are
  encrypted per node with ECIES (X25519, HKDF-SHA256, AES-256-GCM,
  `temps-core/src/ecies.rs`) to the node's registered ingress key.
- Traffic through a Cloudflare Tunnel carries the visitor's address only in
  `CF-Connecting-IP` (ADR 064), and its TLS is terminated by Cloudflare.
- The managed lists are small: Spamhaus DROP 1,593 ranges, Team Cymru full
  bogons about 3,000, DShield 20, Feodo 1 (2026-10-25).

### TLS fingerprints and proxies

**JA4** describes the TLS client, not the person: the same browser build
gives the same value on every connection, while curl, Go, Python, scanners
and each browser family differ. It is computed from the ClientHello, the
first message of a TLS connection, before anything is encrypted.

```
t13d1516h2_8daaf6152771_02713d6af862
│││ │ │ │  │            └ c: sha256(sorted extensions, minus SNI and ALPN
│││ │ │ │  │                 ‖ "_" ‖ signature algorithms in order)[0..12]
│││ │ │ │  └ b: sha256(sorted cipher suites)[0..12]
│││ │ │ └ ALPN, first and last character ("00" if none)
│││ │ └ number of extensions (2 digits)
│││ └ number of cipher suites (2 digits)
││└ d = SNI present, i = no SNI
│└ highest TLS version (13, 12, …), from supported_versions when sent
└ t = TCP, q = QUIC
```

GREASE values are ignored everywhere. The JA4 TLS client fingerprint is
BSD-3-Clause; the rest of JA4+ (JA4H, JA4S, JA4X, …) is under the FoxIO
licence, which restricts commercial use.

A JA4 describes whoever opened the TLS connection that temps accepts. Where
that connection ends today:

| Ingress | Code | TLS | Client address today |
|---|---|---|---|
| Control plane | `temps-proxy/src/server.rs:766–772` (`TlsSettings::with_callbacks(DynamicCertLoader)`) | Pingora, OpenSSL | `resolve_session_client_ip` (`proxy.rs:3858`): trusted loopback proxy → `X-Forwarded-For`; Cloudflare peer → `CF-Connecting-IP`; Bunny peer → `X-Real-IP`; else the peer |
| Worker public ingress | `temps-agent/src/public_ingress.rs:327–400` (`serve_tls`) | tokio-rustls, one request per connection | The peer |
| Worker tunnel origin | `internal_proxy.rs` (`spawn_tunnel_origin`) | none, plain HTTP from local `cloudflared` | `CF-Connecting-IP` |
| temps edge | `temps-edge/src/server.rs:375` | Pingora, OpenSSL | Not forwarded: the origin request carries only `X-Edge-Proxy` and `X-Edge-Region` (`temps-edge/src/proxy.rs:503–508`) |

`handshake_complete_callback` (`server.rs:268`) already stores
`TlsExtensionData { sni_hostname }` in the connection's `SslDigest`, and
`extract_tls_info` reads it back per request (`proxy.rs:1735–1760`).

A proxy that terminates TLS opens its own connection to temps, so temps sees
the proxy's ClientHello: one JA4 for every visitor. Used as the visitor's
fingerprint, it would bind every clearance to the same value and, through
D9, could get the proxy itself challenged. A proxy that forwards TCP without
terminating TLS leaves the visitor's ClientHello intact.

| In front of temps | ClientHello temps sees | Visitor JA4 available from |
|---|---|---|
| Nothing (direct) | visitor's | temps computes it |
| L4 load balancer (TCP passthrough: HAProxy `mode tcp`, Hetzner LB TCP, AWS NLB) | visitor's | temps computes it. The client address then needs the PROXY protocol, which temps does not read today |
| Cloudflare proxy (orange cloud) | Cloudflare's | `cf-ja4`, only on Enterprise with Bot Management, via the managed transform "Add bot protection headers" |
| Cloudflare Tunnel | none (plain HTTP) | `cf-ja4`, same condition |
| Bunny CDN | Bunny's | `CDN-JA4`, on every pull zone |
| Amazon CloudFront | CloudFront's | `CloudFront-Viewer-JA4-Fingerprint` when added to the origin request policy, no extra fee. temps does not trust CloudFront for the client address today |
| temps edge | the edge's | Nothing today |
| Local reverse proxy (nginx, Caddy, Traefik on loopback) | the proxy's | Only if that proxy computes JA4 and sets a header |

The OpenSSL ClientHello callback cannot produce a standard JA4.
`TlsSettings` derefs to `SslAcceptorBuilder` (pingora-core
`listeners/tls/boringssl_openssl/mod.rs:58–70`), so installing one is easy,
but OpenSSL records only the extensions it knows: `verify_extension` returns
no slot for an unknown type (`ssl/statem/extensions.c:498–500`) and
`tls_collect_extensions` stores nothing for it (`:640–650`), so
`SSL_client_hello_get1_extensions_present` and
`SSL_client_hello_get_extension_order` both omit it. Chrome sends extensions
OpenSSL 3.6 does not know (ALPS, the ECH placeholder), so the extension
count and hash would not match the values Cloudflare, Bunny, CloudFront or
public fingerprint lists report.

## Decision

### D1. One verdict per request, with a mode per environment

The proxy computes one **Shield verdict** per request: `block`,
`challenge`, `log` or `pass`, with reasons. Each environment has a **Shield
mode**, inherited from the project like attack mode:

| Mode | block verdict | challenge verdict |
|---|---|---|
| `off` | pass | pass |
| `log` (default) | pass, recorded | pass, recorded |
| `selective` | 403 | challenge |
| `attack` | 403 | challenge, and every other visitor is challenged too |

Attack mode becomes Shield mode `attack`; the existing columns map onto it,
so no project changes behaviour on upgrade. The IP gate stays first: an
operator allow rule beats every Shield verdict, and an operator deny rule is
not a Shield decision.

### D2. Reputation is a signed snapshot pushed to every ingress

The control plane compiles one **Shield snapshot** from these sources, each
with a default verdict the platform admin can change:

| Source | Default verdict | Notes |
|---|---|---|
| `spamhaus_drop` | block | Free list of hijacked and criminal networks |
| `feodo` | block | Botnet command-and-control addresses (abuse.ch Feodo Tracker) |
| `dshield` | challenge | Top attacking networks, rotates daily; off until the operator enables it |
| `fullbogons` | log | Unallocated space; inbound traffic from it is spoofed |
| `iana_reserved` (built in) | log | Ranges that are never a valid public source address |
| `crowdsec` (optional) | challenge | CrowdSec community blocklist, when the operator enrols |
| `operator` | as written | Platform-wide, then per project |
| `observed` | per rule (D9) | Learned across the fleet, addresses and fingerprints |
| `netwatch` (per node) | block | Bans from netwatch on that node (D12) |

Each entry keeps its original range so a match can name it:
`spamhaus_drop (185.93.89.0/24)`. Ingresses load the snapshot into memory;
the lookup costs one `ArcSwap` load and a few binary searches. An ingress
that loses the control plane keeps its last good snapshot; a feed that fails
to download keeps its previous copy.

**Lists are fetched from their publishers, never shipped.** Their terms
decide this:

| List | Terms | So |
|---|---|---|
| Spamhaus DROP | Offered free for any use, but the terms grant no licence to the data and restrict use of the Spamhaus name in commercial materials | Fetched from spamhaus.org by each control plane; not copied into the repository or the binary |
| Team Cymru full bogons | Free to fetch; nothing grants redistribution | Fetched only |
| abuse.ch Feodo Tracker | CC0, commercial use included | Fetched; could be shipped, but rotates within days |
| DShield block list | CC BY-NC-SA 2.5, non-commercial | Defined, off by default; an operator whose use the licence allows enables it |

What every binary does carry is the **built-in baseline**: the IANA ranges
that are never a valid source on the public internet (`0.0.0.0/8`, the three
IPv4 documentation networks, `240.0.0.0/4`, `::/128`, `100::/64`,
`2001:db8::/32` and `3fff::/20`). They are code, not data, so they never go
stale; their verdict is `log`. Loopback, private, link-local, shared (CGNAT)
and unique-local space is left out, because a proxy's own neighbours
connect from there. Every ingress starts from the baseline, and every
snapshot adds to it. The browser JA4 list, verified-crawler domains and
exploit signatures (D8) are also code, versioned with the release.

| List | Verdict | Max age, then `log` | Age counts from |
|---|---|---|---|
| `spamhaus_drop`, IPv4 and IPv6 | block | 30 days | The list's own metadata timestamp |
| `fullbogons`, IPv4 and IPv6 | log | 30 days | Its `# last updated` header |
| `feodo` | block | 7 days | The last fetch that confirmed it |
| `dshield` | challenge | 3 days | The last fetch that confirmed it |
| `iana_reserved` | log | never | — |

An install that cannot reach a publisher therefore stops acting on its old
copy by itself, and an air-gapped install has the baseline only.

### D3. Address and identity come from the connection

Verdicts use the address the proxy already resolves (`resolve_session_client_ip`,
`proxy.rs:3858`; the TCP peer or a trusted tunnel header on workers), plus
the temps edge header of D4. The **visitor identity** that a challenge and a
clearance bind to is computed by the ingress from the connection, never from
a request body:

```
identity = SHA-256( ip_prefix ‖ 0x00 ‖ user_agent ‖ 0x00 ‖ ja4_or_source )[0..16]
ip_prefix     = /24 for IPv4, /64 for IPv6
ja4_or_source = the JA4, or "none:<source>" when there is none (D4)
```

Using the source tag when there is no JA4 means a clearance earned through
one path (say, Cloudflare) does not carry over to another.

### D4. Real TLS fingerprints (JA4), wherever TLS ends

Every TLS ingress computes the client's standard **JA4** from the raw
ClientHello. It is stable across a client's connections while differing
between browsers, HTTP libraries and scanners. Uses:

1. **Identity.** Part of D3, so a stolen clearance cookie fails from a
   different client stack.
2. **Mismatch signal.** A User-Agent that claims a browser while the JA4
   belongs to `curl`, Go, Python or a scanner is a `challenge` reason.
3. **Fingerprint reputation.** The snapshot can list JA4 values with a
   verdict, and D9 promotes fingerprints that attack from many addresses.
   This catches botnets and residential proxies that rotate IPs, which IP
   lists cannot.
4. **Rate keys.** In `attack` mode the rate limiter can key on JA4 as well as
   IP.

**Extraction.** Each ingress reads the first TLS record(s) from the socket
before the handshake, parses the ClientHello, computes JA4 with one shared
implementation, and puts the bytes back so the TLS library reads them
unchanged. No ingress uses its TLS library's view of the ClientHello (see
Context for why OpenSSL's is incomplete).

**The JA4 follows the client-address trust chain.** A per-request
`ClientFingerprint { ja4: Option<Ja4>, source: Ja4Source }` is resolved next
to the address, by the same rules:

| Peer | Address from | JA4 from | `source` |
|---|---|---|---|
| Cloudflare range | `CF-Connecting-IP` | `cf-ja4` | `cloudflare` |
| Bunny range | `X-Real-IP` | `CDN-JA4` | `bunny` |
| Trusted loopback proxy (`trust_loopback_forwarded_ip`) | `X-Forwarded-For` | the configured header, if any (`loopback_ja4_header`) | `local` |
| Registered temps edge | signed edge header | signed edge header | `edge` |
| Any other peer | the peer | computed by this ingress | `direct` |

- When the peer is a trusted proxy and its header is missing, the JA4 is
  `None`. A proxy's own JA4 is never used as the visitor's.
- Forwarded values must match
  `^[tq][0-9a-z]{2}[di][0-9]{4}[0-9a-z]{2}_[0-9a-f]{12}_[0-9a-f]{12}$`;
  anything else is `None`.
- `cf-ja4`, `CDN-JA4`, `CloudFront-Viewer-JA4-Fingerprint`, the configured
  local header and the edge header are removed from every request before it
  goes upstream, next to the existing removal of `CF-Connecting-IP` and the
  other forwarded headers (`proxy.rs:4068–4080`). An app never sees a JA4
  header a visitor could have written.

**temps edges forward the visitor, signed.** The edge computes the visitor's
JA4 the same way and adds one header to its origin request:

```
X-Temps-Edge-Client: v=1; ip=203.0.113.7; ja4=t13d1516h2_…; ts=1793000000; kid=3; sig=<base64url>
sig = HMAC-SHA256(edge_key[kid], "v1|" ip "|" ja4 "|" ts "|" host "|" request_id)[0..16]
```

The origin accepts it only within 60 s of `ts` and with a valid signature
from a registered edge key, delivered with the edge's route sync. This also
fixes the missing client address for traffic through edges, which every
verdict needs.

**PROXY protocol for L4 load balancers, opt in.** A platform setting
`proxy.proxy_protocol_from = ["10.0.0.0/8", …]` makes the pre-TLS read take a
PROXY protocol v1 or v2 header from those peers first, use its client
address, then parse the ClientHello as for a direct connection. Peers outside
the list never get the header parsed. This lets a Hetzner or AWS TCP load
balancer sit in front of temps with full Shield support.

**No JA4 is not suspicious.** Plain HTTP, Cloudflare without Bot Management
and most local reverse proxies give none. Then the identity uses the source
tag (D3), the mismatch rule and fingerprint reputation do not fire, and
nothing is challenged for lacking a JA4. The Shield tab shows coverage per
environment, e.g. "JA4 on 71% of requests: direct 64%, bunny 7%; none 29%:
Cloudflare without Bot Management".

**Guards and limits.**

- All current Chrome builds share one JA4, so a fingerprint alone never
  blocks a common browser; a built-in list of the top browser fingerprints
  can only add `log`.
- Mismatch only adds `challenge`, never `block`: corporate TLS-inspecting
  middleboxes (Zscaler, Netskope, antivirus HTTPS scanning) give a real
  browser a non-browser JA4.
- Tools such as curl-impersonate and uTLS can copy a browser's ClientHello.
  JA4 raises the cost of looking like a browser; it proves nothing alone.
- Every verdict that used a JA4 records it and its `source`.
- The existing `tls_fingerprint` value (a hash of IP and User-Agent) is kept
  for analytics continuity but labelled in the console as "IP and User-Agent
  hash".
- HTTP-level fingerprints (JA4H, HTTP/2 SETTINGS order) are out of scope:
  JA4H is under the FoxIO licence.

### D5. Challenges are signed by the server; clearance is a cookie with a budget

- **Challenge token.** The challenge page carries a token that binds the
  environment, the identity, the difficulty and the issue time, signed with
  HMAC-SHA256 under the current Shield key. Tokens are valid for 5 minutes.
  The token string is the `challenge` input of the existing WASM solver, so
  the solver does not change.
- **Verification.** `POST /api/_temps/captcha/verify` takes only the token,
  the nonce and the reported solve time (`solve_ms`, optional, used only for
  the D6 measurements). The ingress recomputes the identity from the
  connection and checks, in order: MAC, expiry, identity match, then the
  proof-of-work bits. That is one HMAC and one SHA-256 per attempt, with no
  database. The clearance is for the environment named in the signed token,
  not the one the verify request resolved to: on a host with path routes
  (ADR 050) the fixed verify path resolves to the host's environment, while
  the page under `/docs` was served for the routed one. A visitor holds only
  tokens the server issued to them, so this adds no reach.
- **Clearance.** Success sets `__Host-temps_clr_<env_id>`: environment,
  identity, issue and expiry times and a random budget id, signed. One
  cookie per environment, so on a host that serves several through path
  routes, clearing one does not replace another's. `Secure`, `HttpOnly`,
  `SameSite=Lax`, `Path=/`, valid 24 hours. The `__Host-` prefix makes
  browsers refuse a `Domain` attribute, so a sibling subdomain can neither
  set nor shadow it. Any ingress verifies it locally.
- **Request budget.** Each clearance allows a number of requests per hour on
  each ingress (default 3,000; every request counts, page assets included).
  Past the budget, the clearance stops counting and the next page load gets
  a new challenge at higher difficulty.
- **Keys.** Derived from the control plane's master key with
  `EncryptionService::derive_subkey`, one per purpose (challenge,
  clearance) and generation: `temps-shield/<purpose>/k<kid>`. Nothing is
  stored. Every token carries its `kid`; a release rotates by raising the
  current generation, and the previous one stays accepted until its tokens
  expire. Workers, which do not hold the master key, receive the derived
  keys sealed to them (I3).
- `challenge_sessions` leaves the request path in phase 1; the table is
  dropped one release later.

### D6. Difficulty follows the reason and the load, and is measured

- Base difficulty by reason: 18 bits for a first `challenge`, 20 in
  `attack` mode and after a spent budget, 22 after repeated failures.
- **Adaptive:** each ingress compares an environment's request rate and 5xx
  ratio with its last hour; under a spike it adds up to 4 bits, capped at 24.
  This replaces flipping attack mode by hand for most incidents.
- **Measured:** the page reports how long solving took; the ingress records
  it. The target is a median under 2 s on mobile at the base difficulty.

### D7. Only page loads are challenged; exemptions are explicit

A challenge page is served only to `GET` or `HEAD` requests that accept
`text/html`. Any other request with a `challenge` verdict gets `429` with
`Retry-After` and a JSON problem body. Never challenged:

- ACME HTTP-01, the captcha endpoints and the WASM solver;
- **verified crawlers**: a crawler that `crawler_detector` names is verified
  by reverse DNS plus forward confirmation (Googlebot, Bingbot, Applebot and
  others that publish their domains), cached per IP for 24 hours; an
  unverified "Googlebot" is treated as a normal visitor;
- paths the environment marks as exempt (`/api/*`, webhooks, health checks);
- requests with a valid **bypass token** in `X-Temps-Shield-Token`, created
  per project or environment for monitors, CI and API clients;
- a valid clearance within its budget, except for `block`.

### D8. Known exploit requests are blocked at the proxy

The ingress matches each request's method, path, query and a few headers
(`User-Agent`, `Referer`, `X-Api-Version`, `Content-Type`), as sent and after
one and two rounds of percent-decoding, against a table of known exploit
signatures. The table comes from netwatch's `exploits.rs` (about 60 CVEs:
Log4Shell, Confluence OGNL, Apache path traversal, PHP-CGI, Fortinet,
Ivanti and others, plus probes for `.env` and `.git`), moved into a shared
crate. A match blocks the request with 403 in `selective` and `attack` modes,
records the CVE, and counts towards D9. Secret probes count but block only
when the address is also listed. Request bodies are not read in this ADR.

### D9. The fleet learns from every ingress

Every decision other than `pass` is an event (D11). Every 30 seconds the
control plane promotes:

| Evidence in the last hour | Promoted | For |
|---|---|---|
| 1 or more exploit hits from an address | address → `block` | 7 days |
| 3 or more failed or abandoned challenges from an address | address → `challenge` | 24 hours |
| One JA4 with exploit hits from 20 or more addresses, not on the browser list | fingerprint → `challenge` | 24 hours |

An address that probes `/.env` on one ingress is blocked on all of them
within about a minute. Only addresses, fingerprints and reasons cross
tenants; paths and hosts stay with the environment that saw them.

### D10. Workers run Shield too

Shield is one library used by both proxies: the Pingora proxy on the control
plane and the agent's public ingress on workers. A worker that reports the
`shield-v1` capability receives the snapshot and keys, serves the challenge
and verify endpoints itself, and becomes eligible for routes with Shield or
attack mode. The other security settings keep their current control-plane
pinning until they get the same treatment.

### D11. Every verdict explains itself

- Responses carry `X-Temps-Shield: <verdict>; ref=<request id>`. The reason
  is not sent to the client.
- Events go to ClickHouse when it is configured, and every request log gets
  the verdict and reason.
- The console gets a **Shield** tab per environment: mode, counts by verdict
  and source, recent events, solve times, and an address lookup that shows
  the verdict and why ("spamhaus_drop (185.93.89.0/24)", "CVE-2021-41773 on
  /cgi-bin/…").
- Before switching to `selective`, the tab shows how many of the last 24
  hours' requests would have been blocked or challenged.

### D12. netwatch is a source and a consumer, not a dependency

Where netwatch runs on a node, the agent may read netwatch's bans from its
local API and add them to that node's verdicts as the `netwatch` source.
The ingress may send request lines to netwatch over a local socket, which
gives netwatch the HTTPS visibility it lacks. temps works the same without
netwatch.

## Implementation

### I1. Crates

New crate `crates/temps-shield`, with no Pingora, sea-orm or ClickHouse
dependency in its default features, so the agent can use it:

| Module | Contents |
|---|---|
| `snapshot` | `ShieldSnapshot` and `SourceList`: per source, the collapsed networks, delta-encoded (phase 2) |
| `index` | The in-memory form of a snapshot plus the baseline, and `evaluate(ip, now)` (phase 2) |
| `net`, `feeds`, `baseline` | IP networks as integers; the feed definitions and their parsers; the IANA-reserved ranges (phase 2) |
| `verdict` | `evaluate(&ShieldInput, &Index, &EnvPolicy) -> Verdict` |
| `token` | Challenge token and clearance cookie: encode, verify, derived keys (phase 1) |
| `ja4` | ClientHello parser and JA4 string |
| `signatures` | Exploit signatures and matcher, moved from netwatch (MIT/Apache) |
| `gate` | Issue, redeem and admit (phase 1) |
| `redeemed` | Salts of redeemed tokens, grouped by issue minute and dropped once expired (phase 1) |
| `budget` | Per-clearance hourly request counts in a fixed-size count-min sketch (phase 1) |
| `identity` | The D3 visitor identity (phase 1) |
| `crawlers` | Reverse-DNS verification with a moka cache (hickory resolver) |
| `assets` | `challenge.html` (escaped templating) and the WASM solver bytes |
| `server` (feature) | Feed fetchers, compiler, promoter, persistence, API handlers, `ShieldPlugin`. Phase 2 keeps the fetch and compile jobs in `temps-proxy` (`shield/sources.rs`), next to the loader and started by `ProxyPlugin`; they move here once something other than the proxy needs them |

```rust
pub struct ShieldInput<'a> {
    pub ip: IpAddr,
    pub ja4: Option<&'a str>,
    pub user_agent: &'a str,
    pub method: &'a str,
    pub path_and_query: &'a str,
    pub accepts_html: bool,
    pub headers: &'a [(&'a str, &'a str)], // the few D8 reads
    pub clearance: Option<&'a str>,        // __Host-temps_clr_<env_id>
    pub bypass_token: Option<&'a str>,     // X-Temps-Shield-Token
    pub environment_id: i32,
    pub now: i64,
}

pub enum Action { Pass, Log, Challenge { difficulty: u8 }, Block }

pub struct Verdict {
    pub action: Action,
    pub reasons: SmallVec<[Reason; 2]>, // source + matched range / CVE / JA4
}
```

`evaluate` performs no I/O and no allocation on the `pass` path.

### I2. Data model (Postgres)

Migration `mYYYYMMDD_000001_create_shield.rs`:

- `projects.shield_mode TEXT NOT NULL DEFAULT 'log'` (phase 4; until then
  every environment records verdicts and attack mode is unchanged),
  `environments.shield_mode TEXT NULL` (inherit). Backfill `'attack'` where
  `attack_mode` is true. The API writes both columns for one release, then
  `attack_mode` is dropped.
- `environments.shield_exempt_paths JSONB NOT NULL DEFAULT '[]'`.
- `shield_sources (name, enabled, etags, last_attempt_at, last_fetched_at,
  last_error, published_at, entries, sha256, list, created_at, updated_at)`
  (phase 2). One row per feed in `temps_shield::feeds`, which holds the URL,
  format and verdict; `list` is the last good copy, and `updated_at` moves
  only when its networks change. Custom lists (see Later) would add `kind`
  and `url`.
- `shield_operator_entries (id, scope, project_id NULL, network CIDR or
  ja4 TEXT, verdict, note, created_by, created_at, expires_at NULL)`.
- `shield_observed (subject TEXT, kind ip|ja4, verdict, reason,
  first_seen, last_seen, hits, expires_at)`, unique on `(kind, subject)`.
- `shield_snapshots (revision BIGSERIAL, created_at, sha256, body BYTEA,
  sources_updated_at)`, zstd-compressed, keep the last 10 (phase 2).
  `sources_updated_at` is the newest source change a revision was built
  from, which is how the compiler knows a list changed since.
- `proxy_logs.shield_verdict TEXT` and `shield_reasons JSONB`, both null
  when no list holds the client (phase 2).
- `shield_bypass_tokens (id, project_id, environment_id NULL, name,
  token_hash, created_by, created_at, last_used_at, revoked_at)`; tokens
  look like `tst_<id>_<secret>` and are stored as SHA-256, like node tokens.

### I3. Snapshot format and distribution

```rust
pub struct ShieldSnapshot {
    pub revision: u64,
    pub generated_at: i64,
    pub sources: Vec<Source>,          // id, name, kind, default verdict
    pub nets: Vec<NetEntry>,           // network, prefix, source id, verdict override
    pub ja4: Vec<Ja4Entry>,            // fingerprint, source id, verdict
    pub browser_ja4: Vec<String>,      // never blocked on fingerprint alone
    pub envs: Vec<EnvPolicy>,          // id, mode, exempt paths, budget, bypass token hashes
}
```

Phase 2 implements `revision`, `generated_at` and the lists, as
`lists: Vec<SourceList>` (source, verdict, publication time, maximum age and
networks); the other fields arrive with their phases. The body is
serialised with `postcard`. Networks are stored per source,
collapsed, sorted and delta-encoded (a LEB128 start delta and a prefix
byte), and the whole body is compressed with zstd at level 19. Measured on
2026-10-25:

| List | Entries | Plain text | Plain text, zstd | Delta-encoded, zstd |
|---|---|---|---|---|
| Spamhaus DROP IPv4 | 1,593 | 25 KB | 5.7 KB | 3.9 KB |
| Spamhaus DROP IPv6 | 92 | 6 KB | 0.9 KB | under 1 KB |
| Full bogons IPv4 | 3,031 | 48 KB | 7.6 KB | 4.3 KB |
| Full bogons IPv6 | 158,140 | 2.6 MB | 135 KB | 15.9 KB |

In memory, collapsed managed lists are disjoint, so each is one sorted array
of `(start, prefix)`: one binary search gives both membership and the
matching range. IPv6 full bogons dominate at about 5 MB per ingress.
Operator and observed entries may overlap and keep the merged-ranges plus
longest-prefix lookup.

- **Built-in baseline.** `temps_shield::baseline`, compiled into the
  `Index` as revision 0 and kept under every loaded snapshot. No data file,
  generator or refresh workflow is needed while it holds only IANA ranges.
- **Control plane (Pingora).** `setup_proxy_server` creates a
  `ShieldLists`, an `ArcSwap<Index>` starting from the baseline. A loader on
  `security_refresh_runtime()` reads the newest `shield_snapshots` row above
  the loaded revision every 5 s, one indexed read that returns nothing in
  the steady state; it checks the row's sha256, decodes it off the runtime
  threads and swaps the index in. A body that fails either check is skipped
  and the last good index kept.
- **Workers.** The full route snapshot is resent on every 25 s poll, so the
  Shield body does not ride in it. `PublicIngressSnapshot`
  (`route_sync.rs:106–118`) gains:

  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub shield: Option<ShieldRef>, // revision, sha256, derived keys sealed to the node (ECIES)
  ```

  When the revision differs, the agent fetches the body from a new endpoint,
  `GET /api/internal/nodes/{id}/shield/snapshot?revision=N` (node bearer
  token, 304 when unchanged), checks the sha256, swaps it in, and persists it
  next to `routes.json`. Keys are sealed with the existing ECIES session
  under a new HKDF info string, `temps-shield-keys-v1`, and are decrypted
  only into memory, like TLS keys (`route_store.rs:248`). A Shield change
  bumps the route generation so waiting long polls return at once.
- The agent wire DTO (`route_sync_client.rs:56–67`), its store
  (`route_store.rs:65–78`, `apply_public_snapshot` at 186) and `DiskSnapshot`
  (105–113) get the same field.

### I4. Hot path: control plane

In `request_filter`, inside `if let Some(project_ctx)` after the rate
limiter (`proxy.rs:6379`), the attack-mode block (6381–6502) is replaced by
one call:

```rust
let verdict = self.shield.evaluate(&ShieldInput { ip: parsed_ip, ja4: ctx.ja4.as_deref(), .. });
match verdict.action {
    Action::Pass => {}
    Action::Log => ctx.shield = Some(verdict),           // recorded with the request log
    Action::Block => return self.shield_block(session, ctx, &verdict).await,
    Action::Challenge { difficulty } if accepts_html_navigation(ctx) =>
        return self.shield_challenge(session, ctx, difficulty).await, // 403 + page
    Action::Challenge { .. } => return self.shield_too_many(session, ctx).await, // 429 JSON
}
```

`ProxyContext` gains `ja4: Option<String>` and `shield: Option<Verdict>`.

Phase 2 adds only the recording: right after the client address is
resolved, before the IP gate, the lists are evaluated and a verdict other
than `pass` goes into `ctx.shield`, which the request log stores. Nothing
acts on it, and attack mode is unchanged.
The 426 rule for plain HTTP stays only for `attack` mode. The connection
limiter keeps running before the IP gate, as today.

### I5. Hot path: workers

In `internal_proxy::handle`, in the `public_mode` branch after
`store.lookup_public(&host)` (about line 417) and before `proxy_with_retries`
(about 427), the same `evaluate` call runs with the snapshot from the route
store. The agent serves `/api/_temps/captcha/verify` and the WASM files
itself from `temps-shield::assets`, so a worker never needs the control
plane to clear a visitor.

`worker_public_routes()` (`route_sync.rs:501–560`) and
`snapshot_worker_public_routes` (`route_table.rs:3463–3495`) stop excluding
routes for attack mode or Shield when the node reported `shield-v1` in its
heartbeat.

### I6. JA4 extraction

**Parser (`temps_shield::ja4`).** Pure Rust, no dependencies beyond `sha2`.
Implemented in phase 3 as:

```rust
pub struct ClientHello {
    pub legacy_version: u16,
    pub random: [u8; 32],
    pub ciphers: Vec<u16>,                 // GREASE removed
    pub extensions: Vec<u16>,              // in order, GREASE removed
    pub sni: bool,
    pub alpn_first: Option<Vec<u8>>,
    pub supported_versions: Vec<u16>,
    pub signature_algorithms: Vec<u16>,    // in order
}

pub enum Need { More(usize), Done(usize) }      // bytes still needed / bytes the hello takes

pub fn needed(buf: &[u8]) -> Result<Need, ParseError>;
pub fn parse(buf: &[u8]) -> Result<ClientHello, ParseError>;
pub fn ja4(hello: &ClientHello, transport: Transport) -> Ja4;  // Ja4([u8; 36])
pub fn ja4_raw(hello: &ClientHello, transport: Transport) -> String;
impl Ja4 { pub fn parse_forwarded(value: &str) -> Option<Ja4> }  // the D4 format check
```

The hello is owned rather than borrowed: it is parsed once per connection,
and owning it lets a multi-record hello be joined without tying the result
to a temporary buffer. It accepts a ClientHello split across several TLS
records (post-quantum key shares make Chrome's about 1.8 KB) and reads no
more than 16 KB, record headers included. Anything that is
not a TLS ClientHello (SSLv2 hello, plain HTTP on 443, garbage) is a
`ParseError`; the bytes are put back and the TLS library fails the
connection as it does today.

**Control plane and edge (Pingora).** pingora-core 0.9 has a pre-TLS hook
made for the PROXY protocol: `PreTlsProcess::process(&self, stream: &mut
L4Stream)`, installed with `Listeners::set_pre_tls_callback`
(`listeners/mod.rs:149, 481`). `L4Stream::rewind(&[u8])`
(`protocols/l4/stream.rs:522`) feeds bytes back to the next reader.

1. `Ja4Hook::process` (`temps-proxy/src/shield/ja4_hook.rs`) reads the
   PROXY header when the peer is allowed (D4; not yet, see Phases), then
   reads until `ja4::needed` says the hello is whole, and rewinds
   everything it read after the PROXY header. A 2 s timeout covers the whole
   read; on timeout or `ParseError` it rewinds and returns `Ok(())`, so the
   connection behaves as it does today.
2. The JA4 goes into a moka cache keyed by the ClientHello's 32-byte client
   random, 30 s TTL, 100,000 entries.
3. `DynamicCertLoader::handshake_complete_callback` (`server.rs:268`) reads
   the client random from the `SslRef` (`client_random`), removes the entry
   and stores it in a new `TlsExtensionData.ja4` field. From there the JA4
   lives exactly as long as the connection.
4. `extract_tls_info` (`proxy.rs:1746`) runs the trust chain of D4 in
   `resolve_client_fingerprint` and stores the result, JA4 and source, as
   `ProxyContext.client_fingerprint`. It takes the peer and headers the
   client address resolution takes, and the same loopback trust: a trusted
   local proxy gives source `local` and the JA4 in `loopback_ja4_header`,
   or none when that is unset; Cloudflare's addresses give `cf-ja4`,
   Bunny's `cdn-ja4`; every forwarded value is checked with
   `Ja4::parse_forwarded`; anyone else gives the computed JA4.
   `strip_untrusted_client_ip_headers` removes `cf-ja4`, `cdn-ja4`,
   `cloudfront-viewer-ja4-fingerprint`, `X-Temps-Edge-Client` and the
   configured local header before any request goes upstream.

The client-random map exists because `SocketDigest` has no slot for
application data. A client chooses its own random and cannot know another
client's before it is sent, so it can only collide with itself. We also
propose upstream an `extension` slot on `SocketDigest` (like
`SslDigest.extension`); the map goes away if it lands.

The edge installs the same hook in `temps-edge/src/server.rs:375` and builds
`X-Temps-Edge-Client` in its upstream request filter
(`temps-edge/src/proxy.rs:503`).

**Workers (rustls).** In `serve_tls` (`public_ingress.rs:350`), before
`acceptor.accept(stream)`: read the ClientHello with
`ja4::read_client_hello` under the existing 10 s handshake timeout, wrap
the socket in a `Rewind<TcpStream>` (`temps-agent/src/rewind.rs`, an
`AsyncRead + AsyncWrite` that serves the saved bytes first), and pass the
wrapper to `acceptor.accept`. The result is inserted next to
`PublicTlsSni` and `PublicPeer` as a `PublicClientFingerprint` request
extension. Workers serve one request per connection
(`keep_alive(false)`), so no map is needed. The tunnel origin honours a
connector's JA4 header from loopback exactly as it honours its address
header: the connector spec gains `client_ja4` (header and source),
Cloudflare Tunnel's being `cf-ja4` and `cloudflare`, and the origin
removes the header whatever it holds. A bound origin takes changed
headers on the next connector sync, without restarting the connector. Workers remove every header of
`ja4::FORWARDED_JA4_HEADERS` before a request goes upstream, as the
control plane does. Until workers write events (phase 5) they log the
JA4 and its source at `debug` per request.

`ja4::read_client_hello` is shared by both ingresses, behind
`temps-shield`'s `tokio` feature so the parser itself stays free of an
async runtime.

**Settings.**

| Setting | Default | Meaning |
|---|---|---|
| `shield.ja4` | `on` | Compute and record JA4 |
| `loopback_ja4_header` | unset | Header to trust from a loopback proxy, e.g. `x-ja4` |
| `proxy.proxy_protocol_from` | `[]` | Peers allowed to send a PROXY protocol header |

`loopback_ja4_header` is a platform setting next to
`trust_loopback_forwarded_ip` (Settings → Security), refreshed by every
proxy process within seconds like it. Because the proxy strips the header
from every request, the name must be an HTTP token containing `ja4`, so it
can never strip a header an app needs such as `Cookie`; the settings API
answers 400 otherwise.

The raw form (`ja4_r`, the unhashed lists) is logged at `debug` only.

### I7. Tokens and cookies

Implemented in phase 1 (`crates/temps-shield`). All integers big-endian;
`mac` = HMAC-SHA256 truncated to 16 bytes; strings are base64url without
padding.

```
challenge token (60 bytes → 80 chars)
  ver u8 = 1 | kid u8 | env_id u32 | identity [16] | difficulty u8
  | flags u8 | issued_at u32 | salt [16] | mac [16]

proof:     sha256(token_string ‖ nonce_decimal) has ≥ difficulty leading zero bits
validity:  issued_at ≤ now + 30 s skew, and now < issued_at + 300

clearance __Host-temps_clr_<env_id> (56 bytes → 75 chars)
  ver u8 = 1 | kid u8 | env_id u32 | identity [16] | issued_at u32
  | expires_at u32 | budget_id [8] | flags u8 | reserved u8 | mac [16]
```

- The two formats are signed with different keys and have different
  lengths, so neither can stand in for the other.
- Verify request: `POST /api/_temps/captcha/verify` with
  `{ "token": "...", "nonce": "...", "solve_ms": 1234 }`, at most 4 KB,
  answered by the proxy itself on every project host. Answers: `200` with
  the cookie; `403` with `{"ok":false,"error":"expired|replayed|unsolved|invalid"}`;
  `429` `rate_limited` and `503` `busy` (below); `400` for any other body,
  including the old one; `405`, `413`. A failed
  page offers "Try again", which reloads and brings a new token.
- The challenge page carries the token and difficulty in escaped `data-`
  attributes, never in inline script literals, and is sent with
  `Cache-Control: no-store`, since its token is bound to one visitor.
- Each ingress keeps the salts of redeemed tokens, so one solved token
  mints one clearance per ingress. Salts are grouped by the minute their
  token was issued, and a group is dropped once all its tokens have
  expired (at most 6 minutes); salts never leave to make room, since a
  forgotten salt could be redeemed again. Expiry follows the same clock as
  the token checks. One network (an IPv4 /24 or IPv6 /48, whatever the
  User-Agent) may hold at most 256 unexpired salts; past that, verify
  answers 429 `rate_limited`. Once 200,000 unexpired salts are held, verify
  answers 503 `busy` until the oldest group expires. At difficulty 20 a
  token costs about 2^20 hashes, so one GPU could fill the whole set within
  its 6 minutes; the per-network share means filling it takes some 800
  networks instead. Checking and recording a salt
  happen under one lock, taken only for tokens that passed the signature,
  visitor and proof-of-work checks.
- Budget counts are keyed by `budget_id` in a count-min sketch per ingress:
  4 rows of 2^18 lock-free cells, 8 MiB fixed. Each cell carries the hour
  it counts for, so counts start over each clock hour without a sweep.
  Nothing is evicted, so no amount of other traffic gives a spent clearance
  a fresh budget. Clearances that share cells can only push each other's
  counts up, which at worst re-challenges a visitor early. The budget is a
  soft limit: the four rows are not updated together, so concurrent
  requests from one clearance can overshoot it, by at most four times per
  hour. A hard cap (admitting only when every row is under budget) would
  let one clearance far past its budget lock out anyone sharing a single
  cell with it.
- Until JA4 (phase 3), the `ja4_or_source` input of the identity is the
  proxy's existing TLS fingerprint (`tls:<fingerprint>`); attack mode keeps
  answering plain HTTP with 426.

### I8. Events

- `proxy_logs` gets `shield_verdict LowCardinality(String) DEFAULT ''`,
  `shield_reasons Array(LowCardinality(String))`,
  `ja4 LowCardinality(String) DEFAULT ''` and
  `ja4_source LowCardinality(String) DEFAULT ''`, as new entries in
  `storage/clickhouse_migrations.rs`. Phase 2 adds the first two
  (`0005_shield_verdict.sql`), phase 3 the JA4 columns
  (`0006_ja4.sql`, and nullable `ja4` and `ja4_source` on the Postgres
  table). The request log API returns all four. "Would have blocked"
  counts come from here.
- New ClickHouse table `shield_events`, created through `temps-clickhouse`
  migrations: timestamp, project and environment ids, node id, request id,
  ip `IPv6`, ja4, verdict `Enum8`, reasons, signature, host, path, user
  agent, difficulty, solve_ms; `ORDER BY (environment_id, timestamp)`, TTL
  30 days. Without ClickHouse, events go to a Postgres table pruned at 7
  days.
- Control-plane events go through the existing batch writer
  (`proxy_log_batch_writer.rs`, `try_send`, dropped when full).
- Workers write nothing today. The agent batches events (up to 500, or every
  2 s) and posts them gzip-compressed to a new
  `POST /api/internal/nodes/{id}/shield/events`, dropping them when its
  buffer is full.

### I9. Control-plane jobs

`ShieldPlugin`, registered before `ProxyPlugin` (`console.rs:4101`), spawns
`tokio::time::interval` tasks in the style of `temps-audit/src/plugin.rs:54–87`.
Each task takes `pg_try_advisory_xact_lock` so only one control plane does
the work. Phase 2 runs fetch and compile every 5 minutes from `ProxyPlugin`,
which every control plane registers:

| Task | Every | Does |
|---|---|---|
| fetch sources | 1 h | Download feeds with ETag; CrowdSec if enrolled. Each source is claimed by the `UPDATE` that stamps `last_attempt_at`, so no lock is held across a download. A document that is mostly not networks, or a network broader than /8 (IPv6 /16) on a list that acts, is refused and the last good copy kept |
| promote | 30 s | Query the last hour of `shield_events`, upsert `shield_observed`, expire old rows |
| compile | on change, and daily | Build and store a new snapshot revision when a source changed since the newest one; daily so lists dated by their fetch stay fresh. Bump the route generation (phase 5) |
| prune | hourly | Expired observed rows, old snapshots; `challenge_sessions` until it is dropped |

### I10. API and console

- Existing settings endpoints gain fields: `PUT /projects/{id}/settings`
  (`temps-projects/src/handlers/handlers.rs:1901`) and
  `PUT /projects/{id}/environments/{env}/settings`
  (`temps-environments/src/handlers/handler.rs:1333`) accept `shield_mode`
  and `shield_exempt_paths`.
- New, per environment: `GET …/shield/summary?window=24h`,
  `GET …/shield/lookup?ip=…`, `GET/POST/DELETE …/shield/tokens`.
- New, platform: `GET/PUT /settings/shield` (sources, default verdicts,
  CrowdSec enrolment, fleet sharing).
- Console: the "Attack Mode" card in `ProjectSecuritySettings.tsx`
  (341–356) becomes a Shield card with the mode and the "would have" counts;
  `EnvironmentConfigurationCard.tsx` (80–103) gets the inherit selector; a
  new Shield tab shows events, lookup and tokens.

### I11. Testing

- Unit: token round trips, tampering, expiry, wrong identity, wrong
  environment, key rotation; attribution with nested and overlapping ranges;
  signatures against a corpus of normal requests (no false positives) and
  known exploits.
- JA4: reference vectors from the JA4 repository (BSD) and our own captures
  of current Chrome, Firefox, Safari, Edge, curl, Go `net/http`, Python
  `requests` and `httpx`, Nuclei and zgrab2, checked against the values
  Bunny (`CDN-JA4`) reports for the same clients. The parser's first tests
  are the specification's worked example and five real ClientHellos (curl,
  `openssl s_client`, Python `ssl`) with the JA4 and `ja4_r` Wireshark 4.4.7
  computed for each (`crates/temps-shield/tests/data/ja4_wireshark.txt`);
  browser captures follow with the control-plane hook. Fragmented ClientHellos
  (one byte per TCP segment), the 16 KB limit, SSLv2, plain HTTP on 443,
  truncated records. A `cargo fuzz` target on `needed` and `parse` runs
  nightly for ten minutes, and for one on pull requests that touch it
  (`.github/workflows/shield-fuzz.yml`).
- Pingora with the pre-TLS hook still negotiates TLS 1.2 and 1.3, h2 and
  http/1.1, and on-demand certificates; the JA4 reaches every request on a
  keep-alive connection; parse and hash cost under 5 µs per handshake.
- Trust chain: `cf-ja4` from a non-Cloudflare peer is ignored and stripped;
  a Cloudflare peer without it gives `None`, never Cloudflare's own JA4; an
  edge header with a bad signature or an old `ts` is ignored.
- Regression for problem 1: the old body, with a challenge, environment and
  identifier chosen by the client, is rejected; and a token solved for one
  visitor and submitted from another connection (different address prefix,
  User-Agent or fingerprint) sets no clearance. Phase 1 covers both in
  `attack_mode_challenge_tests` and in `temps-shield`'s unit tests.
- Proxy harness tests (`proxy.rs`) for each mode and verdict, and for the
  429 path on non-navigation requests.
- Agent tests: snapshot fetch and swap, sealed keys, challenge served and
  verified on a worker with the control plane unreachable.
- [GoTestWAF](https://github.com/wallarm/gotestwaf) nightly against a test
  environment in `selective` mode, tracking caught attacks and false
  positives over time.
- Load: `evaluate` adds under 20 µs at p99 on the `pass` path.

## Security

- **Keys.** Derived from the master key, never stored; sealed to each
  node's ingress key for workers and held only in memory there. Rotation is
  a new generation in a release, with the previous one still accepted.
  Anyone holding the master key can mint clearances, as they can already
  decrypt every secret.
- **Snapshot integrity.** Ingresses accept snapshots only from the
  authenticated node channel and check the sha256. Feeds cannot create
  operator allow rules or bypass tokens.
- **Verify endpoint.** Stateless, one HMAC and one hash per attempt, covered
  by the rate limiter.
- **Cookie theft.** A stolen clearance works only from the same address
  prefix, User-Agent and fingerprint, for one environment, within its budget
  and expiry. Until phase 3 the fingerprint is the proxy's coarse TLS
  fingerprint (version and negotiated cipher), which many clients share, so
  in phase 1 the binding rests mostly on the address prefix and User-Agent;
  JA4 makes it specific to the client stack.
- **Pre-TLS read.** Bounded at 16 KB and 2 s, and adds no new way to hold a
  connection open: the TLS handshake would wait for the same bytes.
- **Forwarded fingerprints.** JA4 headers are trusted exactly where
  forwarded addresses are and stripped everywhere else, so a visitor cannot
  choose a fingerprint by header. Edge headers are signed and expire in 60 s.
- **Bypass tokens.** Hashed at rest and in snapshots, per project or
  environment, revocable, last use recorded.
- **False positives.** `log` by default, `selective` opt-in, operator allow
  rules win, browser fingerprints never block alone, every block has a
  reason and a lookup.
- **Privacy.** Events hold addresses, fingerprints and paths for 30 days.
  Cross-tenant sharing carries addresses, fingerprints and reasons only.

## Consequences

- Most apps can run `selective` all the time; visitors see a challenge only
  when their address or client has a reason.
- The attack-mode bypass is fixed, and challenges no longer touch the
  database on the request path.
- Protected apps can be served from workers.
- Exploit scanners stop at the proxy for every app, over HTTPS too.
- Bots that rotate addresses are still caught by their fingerprint, on
  direct and TCP-passthrough traffic and behind Bunny, CloudFront (once
  trusted) or Cloudflare Enterprise with Bot Management. Behind Cloudflare
  on other plans and most local reverse proxies there is no JA4, and Shield
  falls back to address and User-Agent.
- Traffic through temps edges gains the real client address.
- Every binary carries only the IANA-reserved ranges. The managed lists
  need the control plane to reach their publishers, with one conditional
  request per list per hour; an install that cannot reach them falls back
  to logging once its copies pass their maximum age.
- The agent gains a snapshot, keys, an event uploader and the challenge
  endpoints; route sync gains one reference and one endpoint.
- Signatures are shared with netwatch and need review in both projects.
- Proof-of-work and fingerprints raise the cost of abuse; they do not stop
  an attacker with real browsers. Rate limits remain necessary.

## Alternatives considered

- **Cloudflare's proxy in front.** Gives a WAF and DDoS protection, but not
  for tunnelled or self-hosted planes without Cloudflare; attackers can still
  reach the origin; temps loses the visitor's address and every reason.
- **Third-party captchas (Turnstile, hCaptcha).** An external dependency and
  a privacy cost on every challenge; the proof-of-work solver exists.
- **Per-request reputation lookups** in ClickHouse or netwatch. A network
  round trip and a failure mode on the hot path.
- **The OWASP Core Rule Set** through a rules engine instead of D8. Much
  broader, but needs a Rust engine or a sidecar and careful tuning against
  false positives. Kept as a later step; D8 starts with specific CVEs.
- **Lists only, as IP-gate deny rules.** Blocks real visitors on listed
  ranges with no way through, and leaves the challenge bypass.
- **JA4 from the OpenSSL ClientHello callback.** Simplest to wire, but omits
  extensions OpenSSL does not know, so the values are non-standard.
- **Switching Pingora to rustls.** The rustls backend does not call
  `certificate_callback` (pingora-core `protocols/tls/rustls/server.rs:75`),
  which on-demand certificates need, and rustls' `ClientHello` exposes no
  extension list either.
- **eBPF capture of the ClientHello** in netwatch. Works for any TLS server,
  but ties a request-path decision to a separate process and a lookup by
  4-tuple. Kept as a netwatch feature for hosts that do not run temps.

## Phases

1. **Fix the challenge** (D3, D5, I7), implemented with this ADR: signed
   tokens, identity from the connection, clearance cookie with a budget,
   escaped template, the regression test. The new `temps-shield` crate
   holds the formats and the gate; the proxy answers the verify endpoint
   itself, and the console's captcha routes and `ChallengeService` are
   removed. Ships alone as a security fix.
2. **Snapshot, sources and the built-in baseline in `log` mode** (D1, D2,
   I2, I3, I8 for the control plane): see what Shield would do before it
   does anything. Workers receive the snapshot in phase 5.
3. **JA4** (D4, I6), recorded in logs and used by nothing yet, in this
   order: parser with vectors and fuzzing; control plane, direct
   connections; forwarded sources (Cloudflare, Bunny, local header) and
   header stripping; workers; temps edges with the signed header; PROXY
   protocol.
4. **`selective` mode** (D6, D7) and the Shield tab (D11, I10), including
   bypass tokens and crawler verification.
5. **Workers** (D10, I5, the event endpoint).
6. **Exploit signatures** (D8) with the shared crate.
7. **Fleet learning** (D9) for addresses, then fingerprints.
8. **netwatch exchange** (D12).

## Later

Left out of this ADR on purpose, to be proposed separately once Shield is in
use:

- **Custom lists.** Operators add their own lists by URL or upload, with a
  verdict, limits on size and coverage, and a preview of what they would
  have matched. Until then, single addresses and ranges go in `operator`
  rules.
- **A Temps Cloud feed.** Planes linked to Temps Cloud receive a signed,
  curated feed (lists, fingerprints, signatures between releases) and may
  opt in to a community list built from what linked planes catch.

## Open questions

- Spamhaus restricts use of its name in commercial materials. Is naming
  the source in a reason (`spamhaus_drop (…)`) in the console and in Shield
  lookups within that, or should temps ask Spamhaus? A redistribution
  licence would also let workers and air-gapped installs carry DROP.
- Should new projects default to `log` or `selective`?
- Can `selective` challenge plain HTTP, now that identity no longer needs
  TLS, or should it redirect to HTTPS first?
- IPv6 binding: /64 or /56?
- May an operator opt out of sharing observations with the fleet, or of
  receiving others'?
- Default request budget per clearance: 3,000 per hour on each ingress in
  phase 1, counting every request; per-environment from its traffic, or
  counting only page loads once D7 lands?
- Do we want JA4H later, under a FoxIO licence?
- Trust CloudFront like Cloudflare and Bunny: its address ranges,
  `CloudFront-Viewer-Address` for the client address, and its JA4 header?
- Should the identity bind to JA4 at all when `source` is a CDN, given that
  CDN-to-origin paths can change between requests?
- Upstream the `SocketDigest` extension slot to Pingora first, or ship the
  client-random map and upstream later?
- Record a proxy's own JA4 as `peer_ja4`, to spot unexpected proxies in
  front of an app?
- QUIC: temps serves no HTTP/3 today; when it does, the same parser applies
  to the QUIC Initial with `q` as the prefix.
