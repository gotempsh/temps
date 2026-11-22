<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Tokenless repository requirements for Temps sandbox service access

Status: proposed cross-repository implementation contract, 2026-11-08.

Parent design: [Time-limited sandbox service access](./sandbox-service-access.md).

Baseline reviewed: Tokenless commit `641a02d63c267b32b0a787cd38de8c99fd500cd3`. These requirements describe work to do in `gotempsh/tokenless`; they have not been filed as issues or implemented. “Verify/extend” means a primitive exists but the required guarantee needs source-level confirmation and conformance tests before reuse. Proposed API names are illustrative, not existing SDK symbols.

## Existing foundation and concrete gaps

| Evidence at the reviewed revision | Integration implication |
|---|---|
| [Workspace manifest](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/Cargo.toml) has reusable Rust crates but `publish = false` | There is no published SDK contract to assume; choose a versioned release or pinned Git consumption strategy |
| [DataPlane::start](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/crates/broker-dataplane/src/lib.rs) accepts a listener configuration and enrolled node identity | Reuse the data plane, but expose embedding lifecycle, readiness, and trusted transport APIs deliberately |
| [broker-control](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/crates/broker-control/src/lib.rs) exposes stores, services, and SecretCipher | Implement Temps adapters rather than embedding the standalone database/UI; verify cipher and audit contracts |
| [SessionService::mint](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/crates/broker-control/src/service/sessions.rs) requires a VaultId; discovery already returns safe service metadata | Native tenant integration needs an explicit mapping or scope-oriented service API; reuse safe discovery |
| [SessionClaims](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/crates/broker-core/src/session.rs) carries one expiry, rule IDs, and labels | Sandbox/node/generation binding and independent grant deadlines must be enforceable claims/policy, not audit labels |
| [RuleScope](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/crates/broker-core/src/rule.rs) matches host, path, method and MCP tools | Git discovery query semantics need an extension or a dedicated protocol validator |
| [GitHub template](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/crates/broker-core/templates/github.yaml) covers API/upload hosts with bearer injection | Add dedicated Git smart-HTTP support; do not widen this template to every GitHub endpoint |
| [Architecture ADR](https://github.com/gotempsh/tokenless/blob/641a02d63c267b32b0a787cd38de8c99fd500cd3/docs/adr/0001-architecture.md) specifies leased snapshots, node delivery, audit backpressure, and native embedding seams | Preserve these guarantees; the ADR alone is not proof of native integration, active-stream expiry, or a complete refresh implementation |

## Required work before the Temps feature can ship

### TL-01 — Supported Rust embedding SDK (new packaging and contract)

- Expose a supported crate/facade or documented public modules for core policies, control-plane services, data plane lifecycle, and conformance tests. Temps must not import private CLI launcher modules or shell out to `tokenless run`.
- Keep the SDK independent of Temps, SQLite, the standalone account system, and UI assets. Make standalone concerns optional features where needed.
- Define SemVer/MSRV, wire and snapshot compatibility, additive-field behavior, and an upgrade/mixed-node-version policy. Unknown security-critical fields or claims must fail closed.
- Publish consumable crate versions with versioned internal dependencies, or explicitly support immutable Git revision dependencies initially. Do not depend on a moving branch. Provide a release path beyond an unpublished path-only workspace.
- Supply a minimal external embedding example and CI that compiles it as an independent consumer.

Acceptance: a small host application starts/stops the proxy and provides in-memory adapters without the standalone server, CLI, or database.

### TL-02 — Native control-plane adapters (verify/extend)

- Document `CredentialStore`, `PolicyStore`, `AuditSink`, `Authorizer`, cipher/key management, and any required node-store contracts and ownership. Expose required types without leaking persistence internals.
- Allow an embedding platform to use its project/environment scope. Either generalize the currently vault-oriented minting API or define a strict adapter mapping without requiring a second user/account system.
- Support encrypted credential references and trusted internal materialization through an adapter. No public secret-read endpoint. Secret-bearing values must be redacted in Debug, errors, tracing, and serialization by default.
- Specify transactional mutation/audit requirements and an outbox integration. Distinguish durable event recording from later delivery to an audit system.
- Provide stable typed errors with identifiers and operation context, without credential values.

Acceptance: Temps-compatible adapters pass `broker-conformance`, including denied cross-scope access, transaction rollback, restart, and duplicate mutation retries.

### TL-03 — Trusted sandbox binding and transport (new supported SDK surface)

- Support host-owned session assignment to an accepted stream/listener or explicit trusted binding object. Ordinary proxy clients must not be able to create that object from headers.
- Bind authorization to tenant scope, sandbox identity, lifecycle generation, and node/audience. Keep transport labels distinct from enforceable claims. Enforce node binding where sessions could otherwise be replayed across brokers.
- Support bounded, independently revocable bindings within one worker process. A localhost port alone is not sufficient isolation on a shared host.
- Accept host-adapted streams suitable for Docker network relays and microVM vsock/tap adapters without coupling Tokenless to Docker, Firecracker, or privileged network setup.
- Keep credentials and session tokens outside guest env, argv, filesystem, and diagnostic output. Strip client-supplied authentication before injecting authorized upstream credentials.

Acceptance: two simultaneous bindings cannot impersonate each other through spoofed headers, replayed sessions, shared NAT, or recycled sandbox/VM identifiers. Destroying a binding invalidates its old generation.

### TL-04 — Grants with independent hard deadlines (new model or equivalent enforceable API)

- Introduce a generic grant binding with ID, scope, allowed rules/operations, generation, absolute deadline, and revoked state. Names and serialization are up to the SDK, but all semantics must be enforced.
- Support multiple services with different deadlines for one sandbox. Renewing a transport session must not renew any underlying grant or widen its rule set.
- Set effective validity to the intersection of grant, session, policy lease, and upstream credential expiry. Validate deadlines locally without a database call on each request.
- Reject ambiguous overlapping credential rules. Define deterministic rule selection and ensure the chosen rule is tied to a live grant; do not union one grant's credential with another grant's permissions.
- Fence stale renewal and revoke/extend races using generations/version comparisons. Expiry has no positive grace period.

Acceptance: service A expires while service B remains usable; an old renewal response cannot resurrect A. Test expiry with controlled clocks, skew and time rollback.

### TL-05 — Revocation of active traffic (verify/extend; release blocker)

- Recheck authorization per request and cancel already-open upstream/downstream streams when a grant is revoked or its effective deadline passes. Cover slow uploads/downloads, CONNECT, keepalive, and supported MCP sessions.
- Expose a revocation result distinguishing accepted, installed, and enforced. Include the policy version and maximum remaining lease window when a node is unreachable.
- Support the proposed Temps bound of at most 5 seconds for connected propagation and 30 seconds under a partition; make lease configuration explicit and validated. Defaults may differ in standalone mode.
- Do not advertise global request limits when counters are per node; support audience pinning or a documented distributed design if a global limit is required.
- Never automatically retry a non-idempotent upstream request after credential refresh or reconnection.

Acceptance: revoke during a large Git transfer and an idle persistent connection; verify termination within the bound. An expired policy stops traffic even if the control plane never comes back. Document that accepted upstream side effects cannot be rolled back.

### TL-06 — Provisioning acknowledgements and safe discovery (extend existing metadata)

- Expose typed status for enrollment, snapshot validity, installed version, lease deadline, credential validity, binding generation, and last safe failure reason.
- Provide an awaitable bounded readiness operation for a particular binding/policy version. A bound TCP listener or live Unix socket does not mean credentials are ready.
- Extend discovery with safe per-grant status and absolute expiry. Never return a provider token, broker session token, raw header, or secret-bearing request URL through discovery.
- Support lifecycle events/callbacks so Temps does not poll every row continuously. Ensure callbacks are bounded and cancellation-safe.

Acceptance: UI adapters distinguish missing configuration, unsupported capability, provisioning, expired access, stale policy, upstream outage, and credential rejection without parsing log messages.

### TL-07 — Git HTTPS policies and templates (new)

- Add GitHub and supported GitLab/self-hosted templates for smart HTTP, using the appropriate Basic/header injection. Keep REST API templates separate.
- Match canonical repository scope and specific RPC endpoints. Distinguish `git-upload-pack` from `git-receive-pack`, including the `service` query in discovery. Reject duplicate/ambiguous query values, encoded separators and path traversal.
- Define repository aliases such as optional `.git` explicitly; normalize before authorization and preserve correct upstream routing.
- Do not forward injected authentication to arbitrary redirects or LFS action URLs. Resolve and validate every new authority independently. LFS is a separate advertised capability until tested.
- Stream pack bodies with bounded buffering. Git traffic must not enter generic JSON/body substitution buffering or whole-body logging.

Acceptance: actual clone/fetch/push with read-only and write grants, cross-repository denial, valid helper-free authentication, expiry during transfer, redirect escape attempts, and no credential bytes in client-visible diagnostics. Test TLS verification with the sandbox CA rather than disabling it.

### TL-08 — Expiring upstream credentials and issuer adapters (new/extend)

- Provide a control-plane credential-provider interface for materializing and refreshing short-lived credentials with exact scope, expiry, and generation. GitHub App installation issuance is implemented by Temps; the SDK remains provider-neutral.
- Serialize refresh per credential with a lease or equivalent single-flight mechanism across replicas. Honor single-use refresh semantics and bound failures/retries.
- Seal only valid short-lived material into the applicable node snapshot. Cap rule validity at credential expiry and atomically replace generations on rotation.
- Scope node delivery to the grants hosted by that trust domain. Avoid sending the full tenant credential set to every worker.
- Keep issuance and refresh out of the request hot path. If refresh cannot complete, return a typed unavailable/expired result rather than injecting stale material.

Acceptance: rotating credentials does not expose old values or widen permissions; refresh outage, provider revocation, duplicate issuers, and stale snapshot delivery fail safely.

### TL-09 — TLS, routing, and egress composition (verify/extend)

- Expose public CA/trust-bundle metadata with version/fingerprint and safe rotation overlap. Private CA keys remain only on trusted nodes.
- Define scope for shared interception keys and node compromise. Document per-trust-domain deployment; per-node constrained intermediates can be a later enhancement.
- Expose integration points for the embedding platform's destination/egress policy. A grant must not bypass that policy, including for explicitly configured private upstreams.
- Preserve authority/SNI/Host checks, DNS rebinding defenses, header stripping, and credential isolation on redirects and upstream failures.
- Allow safe coexistence with ordinary authorized egress without falling back to credential injection after a grant denial. Document unsupported HTTP versions, pinned clients, UDP/QUIC, and raw TCP transports.

Acceptance: conformance covers IPv4/IPv6 private destinations, redirects, alternate ports, mismatched authorities and malicious client credentials. The Temps adapter separately proves network bypass resistance.

### TL-10 — Resource limits, supervision, and audit guarantees (verify/extend)

- Provide explicit `start`, readiness, drain, shutdown, and cancellation semantics. Dropping a failed startup must release listeners and owned tasks. Runtime supervision must report exits to the embedding worker.
- Configure bounded sessions/bindings, connections, per-sandbox fairness, payload buffers, certificate caches, policy sizes, audit queues, and retries. Define behavior at each limit.
- Retain memory-based policy lookup with no per-request DB, filesystem, credential-refresh, or control-plane call. Do not add a global request-path mutex to support bindings.
- Define precisely what audit admission guarantees on process crash, queue saturation, transport outage, and shutdown. Deny injection when required audit admission fails; never claim durability merely because an event entered memory.
- Publish metrics with bounded labels and no tokens, arbitrary URLs, query strings, or secret values. Supply repeatable memory/throughput tests against the parent design's proposed resource budgets.

Acceptance: sustained saturation stays bounded, one sandbox cannot starve all others, shutdown drains or explicitly reports pending audit loss, and cancellation leaves no listener or renewal task behind.

### TL-11 — Compatibility and adversarial conformance suite (extend)

- Add reusable suites for grants, generation fencing, transport binding, active-stream revocation, audit admission, secret redaction, Git policy, and provisioning readiness.
- Use controlled clocks, fake transports, and fault injection for partitions, rollback attempts, out-of-order snapshots, delayed renewals, clock skew, and credential rotation.
- Provide a protocol-version capability handshake. Unsupported critical grant semantics must reject attachment, not degrade to legacy broad sessions.
- Keep a standalone embedding example and a two-sandbox integration harness that requires no production credentials. Templates and fixtures use generic identities.
- Document a migration path for existing standalone sessions, templates, and snapshots if the grant model changes their wire format.

Acceptance: published SDK revision passes conformance and the independent example; Temps pins that revision and runs backend-specific tests against it.

## Work that remains in Temps, not Tokenless

Temps owns service/grant entities and migrations; project/sandbox authorization; provider-connection integration; encrypted persistence adapters; worker placement; Docker network and microVM transport isolation; trusted binding assignment; snapshot/restore reconciliation; API and generated clients; Services/Terminal UI; and host resource supervision. Tokenless must not acquire a dependency on Temps to implement those responsibilities.

The broker SDK is also not a substitute for raw database authentication, network segmentation, provider branch protection, or general exfiltration prevention. Future protocol adapters need their own threat models and tests.

## Suggested implementation order

1. Agree TL-01/02 public contracts and implement the external consumer example.
2. Deliver TL-03/04/05 identity, grant deadlines, and active revocation together; they form one security boundary.
3. Add TL-06 readiness/discovery and TL-08 credential expiry, then TL-07 Git policies.
4. Complete TL-09/10 security and resource validation and TL-11 conformance before declaring the SDK ready for Temps.
5. Integrate into Temps and run Docker plus every advertised microVM backend's acceptance suite. Release readiness requires both repositories' results.

Each requirement should become an implementation issue with its acceptance tests. This list authorizes no issue publication, release, or code changes by itself.
