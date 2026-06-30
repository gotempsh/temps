# ADR-020: Multi-Node Deployment Hardening

**Status:** Accepted — decisions resolved 2026-06-27 (see Decisions); implementation in progress on `harden/multi-node-deployments`
**Date:** 2026-06-27
**Author:** David Viejo
**Companion:** [`020-multi-node-hardening-audit.md`](./020-multi-node-hardening-audit.md) — the full 75-finding security & reliability audit this ADR responds to.

## Context

Temps deploys across a control plane (`temps serve`, Postgres/TimescaleDB) and worker nodes that run `temps join` + `temps agent` (a Docker host). Nodes attach either **Direct** (`--private-address`, operator-managed network) or via **Relay** (a WireGuard mesh brokered through `api.temps.sh`). The control plane (CP) drives each worker's `temps-agent` over HTTP to deploy containers, exec, stream terminals, sync routes/DNS, and hand out secrets. HA managed databases add a second fabric: multi-node Postgres clusters via `pg_auto_failover`.

A fan-out audit (10 dimensions, per-finding adversarial verification, 161 agents) found **75 issues — 7 critical, 27 high, 32 medium, 9 low**. The verifiers for the `secrets`/`lifecycle`/`cluster` dimensions were rate-limited mid-run; those findings were re-verified inline by reading the source directly (all confirmed). The criticals collapse into three root causes plus one structural enrollment weakness:

1. **The agent is root-Docker-over-plaintext-HTTP behind one static, never-rotated bearer token.** `require_agent_auth` (`crates/temps-agent/src/auth.rs:52`) gates *every* agent route on a single SHA-256 token; `service_exec` (`crates/temps-agent/src/service_handlers.rs:650`) runs arbitrary commands as arbitrary users (incl. `root`) in any container; the agent serves bare HTTP and is registered to the CP as `http://…` (`crates/temps-cli/src/commands/join.rs:288`), so the token and all exec/deploy/secret payloads cross the wire in cleartext unless an external WireGuard underlay happens to wrap them. The agent never authenticates the CP, so the CP is impersonable (`agentbnd-1/2/3`, `exec-1/2`, `transport-1/2`).
2. **The overlay/route/DNS data plane is flat.** Blanket nftables `ACCEPT`, every node receives the *entire* cluster route table and the *full* `*.temps.local` DNS zone, and the internal proxy will dial any backend with a client-controllable `Host` (`netiso-1/2/3/4`). There is zero tenant isolation across the mesh.
3. **HA Postgres clusters ship wide-open auth.** `crates/temps-providers/src/externalsvc/postgres_cluster.rs` appends `autoctl_node 0.0.0.0/0 trust`, `pgautofailover_replicator 0.0.0.0/0 trust`, and `host all all 0.0.0.0/0 md5` to `pg_hba.conf`; provisions the tenant app role as `LOGIN SUPERUSER`; and the CP connects with an `AcceptAllVerifier` TLS verifier that returns success for any certificate, with a silent plaintext fallback (`cluster-1/2/3/6`). Data and monitor ports are published on `0.0.0.0` (`service_handlers.rs:113`).

The structural theme cutting across enrollment: **the cluster join token is a single shared, non-expiring, unlimited-use secret**, and **node registration is a name-keyed upsert** (`crates/temps-deployments/src/services/node_service.rs:78-108`) that silently overwrites an existing node's token/address/WG-key. A leaked join token therefore lets an attacker hijack any node's identity and redirect every CP→agent call (`enroll-1/2`, `agentbnd-4`), and pull every tenant's S3 credentials via the unscoped `get_s3_credentials` endpoint (`analyst-1`, `nodes.rs:897-974`).

### What is already done well (and must NOT regress)

The codebase is not naive — the audit also catalogued real strengths the hardening must preserve: constant-time token comparison everywhere (`auth.rs`, `route_sync.rs:280`); join tokens stored only as SHA-256 hashes and shown once; registration **closed by default** when no token is configured; strong CSPRNG for all tokens; **SSRF validation** on `private_address`/`address` (`nodes.rs:496+`); agent config written `0600`/`0700`; the CLI **refusing the server's `insecure_tls` opt-in** for the token-carrying join call; agent tokens **encrypted at rest** (AES-256-GCM); the public exec API correctly `permission_guard!`-ed and DB-ownership-verified (`crates/temps-deployments/src/handlers/container_exec.rs`); and **ECIES-encrypted** TLS-cert distribution to edge nodes (private keys never reach workers). The plan below extends these patterns rather than replacing them.

### Design constraints

- **Single self-contained binary.** No new always-on external dependency (no service mesh sidecars, no external CA service). New crypto/PKI lives in-process.
- **Self-hosting cannot trust Temps Cloud.** The relay (`api.temps.sh`) must become an untrusted rendezvous, not a trust anchor. The security of a self-hosted mesh must not depend on Temps' infrastructure.
- **Don't break existing fleets or the git-push DX.** Every change ships behind a fail-safe default and an explicit, operator-controlled enforcement switch, with an upgrade path that lets already-joined nodes re-credential without a flag day.
- **Config is entity state, not env vars** (per `CLAUDE.md`): new runtime toggles are columns on `settings.multi_node` (or new tables), changeable per-record via API/UI with audit logging — never `TEMPS_*` env vars.
- **Every write is audit-logged.**

## Decision

Adopt a **zero-trust posture for the multi-node fabric**: treat every node as a potential adversary, the relay as untrusted, and the transport as hostile. Defense-in-depth replaces the current single-secret, single-channel trust model. The work is organized into seven workstreams (WS), each mapping to a finding cluster.

### WS-1 — Node identity & enrollment (`enroll-1..8`, `agentbnd-4`, `secrets-2/5`, `wg-5`)

1. **Replace the shared eternal join token with short-lived, single-use, scoped enrollment tokens.** New table `node_enrollment_tokens` (`token_hash`, optional `bound_node_name`/`bound_labels`, `max_uses`, `used_count`, `expires_at`, `created_by_user_id`, `revoked_at`, `ca_fingerprint`). An admin mints a token per node (or per batch) from Settings → Worker Nodes / CLI; it expires (default 1h) and is consumed on use. The legacy single shared token keeps working behind `settings.multi_node.legacy_shared_token_enabled` (default `true` for upgrade, flips to `false` for new installs), deprecated and warned-on.
2. **Bind node identity to a first-seen credential.** On first registration, persist the node's `identity_public_key` (WireGuard pubkey and/or the client-cert public key from WS-2) immutably. Re-registration that changes `token_hash`/`wg_public_key`/`address` for an existing name MUST prove possession of the prior token (or the prior client cert), else it is rejected with 409 and audit-logged. This closes the name-keyed-upsert hijack (`enroll-1`, `agentbnd-4`) at the service layer (`node_service::register`).
3. **Rate-limit `/internal/nodes/register`** (per source IP + global) and audit-log every enroll / reconnect / remove (`enroll-3/7`).
4. **Full credential revocation on node removal:** delete the node token, **tear down its WireGuard peer**, and **free its overlay CIDR** (`enroll-6`, `wg-3`, `lifecycle-1`) — currently removal leaves all three live.

### WS-2 — Agent transport & mutual authentication (`agentbnd-1/2/3/5/6`, `exec-2/8`, `transport-1..7`, `secrets-1`)

1. **mTLS between CP and agent, anchored in a per-cluster CA.** When multi-node is first enabled the CP generates a cluster CA (cert + key stored encrypted via `EncryptionService`). At enrollment the node generates its own keypair + CSR; the CP issues a per-node leaf cert. Thereafter the agent serves TLS with its leaf and the CP presents a client cert the agent pins to the cluster CA. The bearer token becomes a **second factor**, not the only secret. This simultaneously: authenticates the CP to the agent (kills `agentbnd-3`), encrypts the channel even in Direct mode without WireGuard (kills the cleartext criticals `agentbnd-2`/`transport-1/2`), and gives a basis for per-node revocation.
2. **Make the relay untrusted.** The admin-minted enrollment token embeds the CP's **CA fingerprint**; the node verifies the CP's identity against it out-of-band, so a malicious relay cannot substitute its own WireGuard pubkey or certificate (`wg-1`, `transport-3`). The **agent token is always node-generated** — fix relay mode so `api.temps.sh` never mints it (`transport-3`, `enroll-8`).
3. **Bind the agent to the overlay/WG interface by default**; refuse to start bound to a public interface without an explicit `--insecure-agent-bind` acknowledgement (`agentbnd-6`).
4. **Replay protection** on state-changing agent calls (exec/deploy): timestamped, HMAC-signed requests with a short freshness window (or TLS channel binding) (`agentbnd-1`).
5. **Remove the global `insecure_tls` escape hatch** from the cluster + remote-deployer clients, or gate it behind a loud, per-connection, audit-logged opt-in (`exec-8`, `transport-5`). Add connect timeouts (`transport-6`).
6. **Persist the WireGuard private key** (encrypted, `0600`) so a worker reboot reestablishes the tunnel instead of silently losing it (`wg-4`); add a **preshared key** per peer for defense-in-depth (`wg-2`).

### WS-3 — Data-plane tenant isolation (`netiso-1/2/3/4/5/6/8`)

1. **Scope route and DNS snapshots per node.** A node receives routes/zones only for the environments actually scheduled on it; the CP computes per-node route sets instead of serving the whole table/zone (`netiso-2/3`). This is the single highest-leverage isolation fix.
2. **Default-deny overlay segmentation.** Replace the blanket nftables `ACCEPT` with default-deny + explicit allow for same-tenant container CIDRs and the proxy hop only (`netiso-1`).
3. **Harden the internal proxy:** allowlist backend addresses to known container endpoints, and **strip inbound `x-forwarded-*` / `x-temps-*` headers** before forwarding so overlay clients can't spoof identity to backends (`netiso-4/5`).
4. **Status-check node-token auth:** reject tokens belonging to deleted/draining/decommissioned nodes (`netiso-6`); enforce snapshot freshness/drift (`netiso-8`).

> **Revision 2026-06-27 — WS-3 reassessed against the intended routing model.**
> The data plane is **deliberately Kubernetes-style: every node must accept ingress for *any* domain and proxy it cross-node to wherever the container runs** (the overlay makes any container reachable by IP from any node). This changes two items above:
> - **WS-3.1 (per-node route/DNS scoping) is WITHDRAWN — it is incorrect for this architecture.** Because external/LB traffic for environment `Y` can legitimately land on a node that does **not** run `Y`, that node *must* hold `Y`'s route and resolve `Y`'s internal service DNS, or the request 404s. The cluster-wide route table and `*.temps.local` zone are therefore **by design, not a leak** (`netiso-2/3` re-classified as accepted risk). An `EdgeRouteEntry` carries only `domain → (project, environment, is_static)` — no backend address/secret — so the information exposure is the domain inventory only.
> - **WS-3.2 (default-deny overlay) is DEFERRED by product decision (no tenant restrictions for now).** The flat overlay is retained, matching Kubernetes' own default (allow-all until a NetworkPolicy is added). When pursued, it is the true K8s-NetworkPolicy analogue and is **not CIDR-expressible**: `compute_cidr` is allocated **per node, not per tenant**, so multiple tenants share a node's /24. Enforcement requires the CP to compute per-node `container-IP → tenant` allow-sets and the agent to install nftables sets (default-deny forward + same-tenant/proxy/DNS/established/egress allows), shipped behind a `settings.multi_node` opt-in (off on upgrade, on for new installs) and verified on DinD (same-tenant cross-node passes, cross-tenant blocked).
>
> **Net WS-3 status:** 3.1 withdrawn; 3.2 deferred; **3.3 header-strip shipped** (`netiso-5`, internal_proxy strips client `x-forwarded-*`/`x-temps-*`) with backend-allowlist still open; **3.4 shipped** (`edge_routes` rejects non-`active` node tokens, `netiso-6`).

### WS-4 — Secret distribution to nodes (`analyst-1`, `secrets-1/3/4/6`)

1. **Scope `get_s3_credentials`** to sources the node legitimately needs (referenced by a backup/restore job currently assigned to that node, or bound to the node's project set); 403 + audit otherwise. Prefer minting **short-lived, source-scoped** credentials per backup job over handing over long-lived root S3 keys (`analyst-1`).
2. **Route credential-like env values** (URLs with embedded passwords, `*_KEY`/`*_SECRET`/`*_TOKEN`/`*_PASSWORD`) through the file-based `/run/secrets` (`0400`) path instead of Docker env, so they aren't readable via `docker inspect` / `/proc/<pid>/environ` (`secrets-6`).
3. **Authenticate the agent Swagger/OpenAPI surface** or disable it in release builds (`secrets-4`, `transport-7`).
4. Agent/node **token rotation + revocation** path (shared with WS-1/WS-2) so compromise isn't permanent until manual node deletion (`secrets-3`, `enroll-5`, `exec-6`).

### WS-5 — Lifecycle, failover & scheduler resilience (`lifecycle-1..8`)

1. **Wire `check_drain_completion` into the 60s health loop** in `crates/temps-cli/src/commands/serve/console.rs` so drains auto-complete instead of hanging in `draining` forever (`lifecycle-7`).
2. **Single-flight / leader-election the failover loop** so the split proxy/console topology (ADR-017) doesn't run concurrent double-redeploys (`lifecycle-4`).
3. **Bidirectional reconciliation:** when a node returns from `offline`, stop orphaned containers the CP no longer tracks, preventing duplicate live replicas (`lifecycle-2`).
4. **Retry failover redeploy with backoff** and surface stuck/best-effort states instead of silently stranding workloads (`lifecycle-3/8`).

### WS-6 — HA Postgres cluster hardening (`cluster-1/2/3/5/6/7/8`)

1. **Scope every appended `pg_hba` rule to the cluster overlay subnet** instead of `0.0.0.0/0`; bind monitor/data ports to the overlay interface, not `0.0.0.0` (`cluster-1/2`).
2. **Drop replication `trust`** in favour of cert/SCRAM replication auth; use `scram-sha-256`, not `md5` (`cluster-2`).
3. **Stop provisioning the tenant app role as `SUPERUSER`** — grant least privilege (own database, `CREATE` on its schema); keep a separate break-glass superuser not exposed by the catch-all rule (`cluster-6`).
4. **Pin the cluster CA** (verify against a stored per-cluster root) instead of `AcceptAllVerifier`, and remove the silent plaintext fallback for cluster connections (`cluster-3`).
5. **Fence on failover / split-brain** and bind member identity to a node-ownership check rather than trusting `node_id` as-is (`cluster-5/7/8`).

### WS-7 — Install/join bootstrap & supply chain (`supplychain-1..8`)

1. **Verify the published `.sha256` (and ideally a minisign/cosign signature) before executing any downloaded binary**; fail closed when the checksum asset is absent — no silent downgrade-to-no-verify (`supplychain-1/8`).
2. **Pass the join token via env/stdin/file, never a positional CLI arg** (it currently leaks via `ps` and shell history) (`supplychain-5`).
3. **Keep the DB password out of the systemd `ExecStart` line** — use `EnvironmentFile=` (`0600`) (`supplychain-2`).
4. **Harden generated systemd units**: `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, restricted `ReadWritePaths` (`supplychain-3`).
5. **Verify the Docker install** instead of piping `get.docker.com` to root, and **enforce `https://` for the control-plane URL** (`supplychain-4/7`).

## Data model & crate changes

- **New table** `node_enrollment_tokens` (WS-1) — migration `m20260627_000001_node_enrollment_tokens`.
- **New columns on `nodes`** (WS-1/WS-2): `identity_public_key`, `client_cert_fingerprint`, `enrollment_token_id`, `cert_serial`, `cert_not_after` — migration `m20260627_000002_node_identity_columns`.
- **New columns on `settings.multi_node`** (JSON sub-struct, no env vars): `legacy_shared_token_enabled`, `require_mtls`, `enforce_node_isolation`, `relay_trust_mode`.
- **New PKI capability** — a `temps-node-pki` module (or extend `temps-core`) for the per-cluster CA: generate/store (encrypted) the CA, issue/sign per-node leaf certs, expose the CA fingerprint. Reuses `EncryptionService` for at-rest key protection; no external CA dependency.
- **Touched crates:** `temps-agent` (mTLS server, interface bind, replay), `temps-deployer` (mTLS client, drop `insecure_tls`), `temps-deployments` (enrollment/identity service, per-node route/DNS scoping, S3 authz, lifecycle loop), `temps-wireguard` (PSK, key persistence, peer teardown), `temps-network` (default-deny, per-node CIDR), `temps-dns-resolver` (scoped zones), `temps-providers` (`postgres_cluster.rs` pg_hba + role + TLS), `temps-query-postgres` (CA pinning), `temps-config` (settings + enrollment tokens), plus `worker.sh`/`install.sh`/`deploy.sh`.

## Rollout & backward compatibility (fail-safe → enforced)

Each enforcement switch ships **observe-then-enforce**:

- **Phase A (ship, default off):** new code paths land; `require_mtls`, `enforce_node_isolation` default `false`; the CP auto-issues a CA and per-node certs at the next reconnect so already-joined nodes silently gain mTLS material without operator action. Legacy shared token still accepted.
- **Phase B (warn):** the CP surfaces which nodes still lack mTLS / are on the legacy token; admin UI nudges rotation.
- **Phase C (enforce):** once all nodes report ready, the operator flips `require_mtls` / `enforce_node_isolation` / `legacy_shared_token_enabled=false`. New installs default to enforced.

The criticals that are *pure tightenings with no client-side change* — S3 authz (WS-4.1), the name-upsert identity fix (WS-1.2), `check_drain_completion` wiring (WS-5.1), supply-chain checksum verification (WS-7) — ship enforced immediately; they don't require a node handshake change. (pg_hba subnet scoping is the same shape but lives in deferred WS-6.)

## Phasing (by severity / dependency)

Per Decision 5, the **first PR** carries P0 + cohesive P1. **WS-6 is deferred** (Decision 4); **relay hardening is parked** (Decision 2).

- **P0 — close the criticals (first PR):** WS-2.1 (mTLS + per-cluster CA + CSR enrollment), WS-3.1+3.2 (per-node snapshots + default-deny, opt-in/new-on), WS-4.1 (S3 authz scoping), WS-1.2 (identity binding / kill name-upsert hijack).
  - *Self-contained tightenings landed first within the PR (no handshake change, low risk):* WS-4.1 S3 authz, WS-1.2 name-upsert fix, WS-1.3 registration rate-limit + audit, WS-5.1 `check_drain_completion` wiring, WS-7 supply-chain script hardening.
- **P1 — highs (same PR where cohesive):** rest of WS-1 (scoped/expiring enrollment tokens, revocation-on-remove), WS-2.3-2.6, WS-3.3-3.4, WS-5.2-5.3.
- **P2 — mediums:** token rotation, leader election, PSK, WG key persistence, header stripping.
- **P3 — lows:** swagger auth, connect timeouts, `next_available_ip` mask fix (`wg-7`, *disputed*).
- **Deferred (separate future work):** WS-6 HA Postgres cluster hardening (until clustering ships), relay-mode untrusted hardening, remaining cluster fencing.

## Consequences

### Positive
- A leaked node token or a malicious relay no longer yields worker takeover or cross-tenant secret theft: mTLS + identity binding + scoped secrets shrink the blast radius from "the whole fleet" to "one node's own scheduled workloads."
- The data plane gains real tenant isolation; a compromised container can no longer read every tenant's routes/DNS or proxy to arbitrary backends.
- HA database clusters stop being internet-reachable with `trust`/SUPERUSER (deferred to WS-6, since clustering isn't yet in use) — but the fix is scoped and ready when clustering ships.
- Self-hosters get a security model that does not trust Temps Cloud, reinforcing the core "own your infra" positioning.
- Most strengths (constant-time compares, SSRF guards, ECIES cert delivery, encrypted-at-rest tokens) are reused, not rebuilt.

### Negative
- A per-cluster CA + per-node certs add real complexity: issuance, storage, rotation, and an expiry/renewal surface that itself must be monitored (a new failure mode — an expired node cert breaks the agent channel).
- mTLS + default-deny isolation are the kind of changes that can wedge an existing fleet if enforced too early; the observe-then-enforce rollout is mandatory, not optional, and adds release coordination.
- Per-node route/DNS scoping moves work from "broadcast everything" to "compute per node," adding CP-side cost and a correctness surface (a scoping bug = a missing route = a down deployment).
- Tightening pg_hba/role privileges may break operators who *rely* on direct external DB access today; needs a documented migration and a release note.

## Alternatives considered

- **Adopt an off-the-shelf mesh (Istio/Linkerd) or SPIFFE/SPIRE.** Rejected: weight, Kubernetes-centricity, and an external control plane contradict the single-binary ethos. We adopt the *idea* (per-node short-lived identities ≈ SVIDs) without the dependency.
- **Tailscale / Headscale instead of embedded WireGuard.** Rejected as a default (external dependency, contradicts self-contained install), but worth documenting as a supported operator option for those who already run it.
- **Keep the relay as a trust anchor but audit/log it.** Rejected: a self-hosted security model must not depend on trusting Temps' infrastructure; the relay must be reduced to an untrusted rendezvous.
- **Signed-token-only (HMAC, no mTLS).** Cheaper, but doesn't authenticate the CP to the agent or encrypt the channel in Direct mode; keeps the cleartext exposure. **Rejected per Decision 1** in favour of K8s-style mTLS.

## Not solved by this ADR

- **Container-escape blast radius** — a deployed app sharing the host with the agent + Docker socket. Reducing agent privilege (WS-2.3 / running non-root) helps, but full isolation (rootless Docker, gVisor/Kata) is a separate ADR.
- **Control-plane HA / SPOF** — this ADR hardens the *fabric between* CP and nodes, not the CP's own single-node-ness.
- **Cross-node clock & cert-expiry monitoring**, agent **auto-update signing** as a first-class channel, and a full **per-tenant network-policy engine** — flagged in the audit's completeness note, deferred to follow-ups.

## Decisions (resolved 2026-06-27)

1. **mTLS, modeled on Kubernetes TLS-bootstrapping.** Chosen over signed-token-only. The node uses a short-lived enrollment token to submit a CSR; the per-cluster CA returns a per-node client cert (private key never leaves the node); the node verifies the CP via the CA fingerprint carried in the enrollment token; thereafter the channel is mutual TLS and the node identity scopes what it may fetch. This is the K8s bootstrap-token → CSR → `system:node:<name>` + Node-authorizer model, minus the external control plane. It gives rotation and node-scoped secrets as a side effect.
2. **Direct + mTLS is the supported secure topology; relay-mode hardening is parked.** With mTLS, Direct mode is encrypted and mutually authenticated even over a hostile network, which removes most of relay mode's reason to exist for self-hosters. The relay stays a Temps Cloud managed convenience (Temps controls both ends); WS-2.2's "untrusted relay" work is deferred. Tradeoff documented: self-hosters with NAT'd, mutually-unreachable workers must supply their own VPN/WireGuard.
3. **Default-deny isolation: opt-in for existing installs, on-by-default for new installs.** `enforce_node_isolation` defaults `false` on upgrade and `true` for fresh installs, via the observe-then-enforce rollout.
4. **WS-6 (HA Postgres cluster hardening) is deferred — `topology: cluster` is not in real use yet.** It is NOT in the first PR. It must land before HA clustering is offered to any operator (the `0.0.0.0/0 trust` + `LOGIN SUPERUSER` exposure is critical *if used*), but carries no urgency today.
5. **One large PR.** Land as much of P0 + cohesive P1 as fits in a single reviewable change; no artificial per-workstream split.
