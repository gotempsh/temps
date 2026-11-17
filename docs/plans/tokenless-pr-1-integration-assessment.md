<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Tokenless PR #1: Temps integration readiness

Assessment date: 2026-11-08. Decision: **sufficient foundation to start integration, insufficient to enable the complete time-limited sandbox access feature.**

Compared [Tokenless PR #1](https://github.com/gotempsh/tokenless/pull/1) at `bd4ca27af474592b60354fc7343b3a9b1153f3c1` with Temps at `9b4064571835c3a777ea9906b117f3259cf8e3eb`, the [original requirements](./tokenless-sdk-requirements.md), and the [Temps design](./sandbox-service-access.md). At inspection the Tokenless PR was open and all seven reported CI checks passed, including the external embedding example, minimum Rust version, and Linux/macOS Rust jobs. This assessment reads pinned source and tests; it does not independently execute Tokenless tests or prove Docker/microVM integration. A green PR is not proof that every Temps requirement is implemented.

## What PR #1 now provides

The prior requirements document describes an older baseline. These capabilities no longer need to be requested from scratch:

| Contract | Implemented evidence | How Temps can use it |
|---|---|---|
| Rust embedding | `tokenless-core`, `tokenless-control`, `tokenless-dataplane`, `tokenless-conformance`; external `examples/embedded-host` | Embed public crates without the standalone broker server/UI |
| Host-supplied snapshots and audit shipping | `DataPlane::start_embedded`, `SnapshotInstaller::install`, `AuditShipper` | Trusted worker installs signed, node-specific policy; Temps supplies audit delivery |
| Trusted transport bindings | `BindingSpec`, `DataPlane::bind`, generic `Binding::serve` | Feed host-authenticated Docker relay or microVM streams; guest headers cannot select the session |
| Scope/node/incarnation claims | `SessionClaims.aud`, `.sub`, `MintSession.node_id`, `.binding` | Mint a session for one worker and sandbox incarnation; always set both optional fields |
| Session lifetime cap | `MintSession.not_after` | Cap every mint/renewal at the persisted grant expiry; this alone does not hard-stop active traffic |
| Status and lifecycle | `Binding::ready`, `Binding::status`, `DataPlane::events`, `shutdown`/`ShutdownReport` | Wait for a minimum installed version; expose typed readiness and shutdown outcomes |
| Upstream credential expiry and restricted delivery | credential `not_after`, rule snapshot expiry metadata, node-scoped rules | Rotate through control services and restrict what credentials each worker receives |
| Git authentication foundation | `github-git` and `gitlab-git` Basic-auth templates; streaming tests | Compile restricted repository policies rather than using the templates' broad default paths |
| Conformance | store suite and `dataplane_suite`, including binding isolation, generation checks, revocation, lease expiry and diagnostic checks | Run against Temps adapters once they exist |

Primary source entry points: [data plane](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-dataplane/src/lib.rs), [bindings](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-dataplane/src/binding.rs), [minting](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-control/src/service/sessions.rs), [stores](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-control/src/store.rs), [conformance](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-conformance/src/dataplane.rs).

## Remaining Tokenless contracts

### T1 — Hard expiry for active traffic (release blocker; TL-04/05/08)

`OPEN_TRAFFIC_GRACE_SECS` is 3600. `SessionWatch::new` uses `open_traffic_deadline(claims.exp)`, so streaming responses and passthrough tunnels may survive session expiry for an additional hour while policy leases remain fresh. New requests are rejected at expiry, which is useful but weaker than the promised “access for X time.” Credential `not_after` is checked before injection; the active-stream watch does not include that credential deadline.

Provide an explicit strict mode or independent grant deadline that cuts active traffic at the minimum of grant expiry, session expiry, policy lease and applicable credential expiry. Standalone renewal grace may remain a separate mode. A host timer that revokes a whole binding is not a substitute for SDK-enforced per-grant expiry under restart, partition, or multiple service deadlines. Renewal must never extend the original grant deadline.

Acceptance: slow upload/download, idle CONNECT and persistent HTTP streams stop at the hard deadline with fresh snapshots continually arriving; service B stays usable after A expires; delayed renewal and clock changes cannot revive A. Test credential expiry during a transfer separately.

Evidence: [session constants](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-core/src/session.rs), [active traffic cancellation](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-dataplane/src/cut.rs), [credential expiry check](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-dataplane/src/proxy.rs).

### T2 — Independent grants on the same authority (release blocker for general multi-service access; TL-04)

A binding supports up to 16 sessions, but `overlap` rejects sessions whose host patterns overlap, and `session_for` chooses by host/port before the request path is known. Two grants for `https://api.example.com/team-a` and `/team-b` cannot carry independent deadlines on that binding. The new most-specific rule selection inside a session does not solve independent session deadlines. Combining both grants into one session and taking the shortest expiry would unexpectedly terminate the longer grant; taking the longest would widen access.

Provide per-rule grant identity/deadline enforcement inside one session, or request-level selection across sessions using complete canonical request scope. Also expose a version-fenced replacement/update contract for adding/removing grants and renewing sessions on an existing binding. The current binding stores an immutable token vector; replacing and revoking the entire binding interrupts unrelated traffic. Temps must prevent stale binding commands from reactivating an older incarnation regardless of SDK support.

Acceptance: two paths/repositories on the same host have different deadlines and credentials; expiry/revocation/renewal of either cannot borrow the other's permissions or disrupt unrelated allowed streams. Explicitly enforce the session-count limit in the host/UI if it remains.

### T3 — Canonical Git smart-HTTP policy (release blocker for Git; TL-07)

The Git templates provide correct injection mechanisms, but both default to `/*` with GET/POST. `RuleScope` matches host/path/method and optional MCP tools, with no query predicate. It cannot distinguish `info/refs?service=git-upload-pack` from `info/refs?service=git-receive-pack` at discovery. Denying the subsequent push RPC by path is helpful, but does not meet our full discovery contract.

Add a supported Git policy compiler/validator or query-aware scope extension. Require canonical repository identity, explicit `.git` alias behavior, separate read/write RPCs, and rejection of duplicate/ambiguous discovery parameters. Keep LFS unsupported unless separately validated; never forward injected credentials to an arbitrary redirect or LFS action authority.

Acceptance: real clone/fetch/push against a local Git HTTP fixture, read-only push denial including discovery, cross-repository denial, encoded path/query attacks, redirect isolation, TLS CA verification and bounded pack streaming. Existing HTTP-shaped Git tests should not be presented as an actual Git-client end-to-end run without that evidence.

Evidence: [GitHub template](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-core/templates/github-git.yaml), [GitLab template](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-core/templates/gitlab-git.yaml), [RuleScope](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-core/src/rule.rs).

### T4 — Enforcement acknowledgement for revocation (TL-05/06)

`Binding::revoke()` returns immediately after signalling cancellation. `SessionService::revoke()` persists revocation and notifies policy listeners; its return is not proof that a remote worker has stopped forwarding. Readiness/version APIs already help with installation, but a synchronous signal is not an awaitable completion barrier for existing traffic.

Expose or document a bounded awaitable per-binding/per-session enforcement result, including the installed version, stopped connections and remaining lease bound for unreachable nodes. Temps can aggregate worker acknowledgements and own the durable workflow; Tokenless should supply a local primitive whose completion has a precise meaning. Enforce the design's connected propagation target of 5 seconds and partition bound of 30 seconds through validated lease settings. Never claim already-accepted upstream effects can be undone.

Acceptance: revoke while actively streaming and while idle; completion is reported only after forwarding has stopped, including under delayed installation and disconnected control-plane delivery.

### T5 — Complete secret-safe SDK diagnostics (TL-02/03)

`BindingSpec` redacts tokens, but `MintedSession` derives `Debug` with a plaintext `token: String`. `JoinToken` and `EnrollRequest` also derive `Debug` over secret material. This is a source-level diagnostic exposure, not evidence of an actual leak in Temps.

Use secret-bearing wrappers or hand-written redacting Debug for all public secret-holding SDK types. Preserve explicit one-time serialization where the standalone API intentionally returns a freshly minted credential; do not silently remove that API. Test Debug/errors/tracing independently from API serialization.

Evidence: [MintedSession](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-control/src/service/sessions.rs), [enrollment types](https://github.com/gotempsh/tokenless/blob/bd4ca27af474592b60354fc7343b3a9b1153f3c1/crates/tokenless-core/src/protocol.rs).

### T6 — Credential-provider and refresh ownership (TL-08)

The PR adds expiry metadata and rotation, but I did not find a public credential-provider/refresh coordinator contract. OAuth template metadata alone does not implement issuance or replica-safe refresh.

Either provide that contract in Tokenless, or explicitly support a host-owned coordinator that calls the existing rotation API. **Temps owns GitHub App issuance, provider authorization and encrypted durable state.** Specify idempotent rotation, version fencing, single-flight refresh across replicas, failure behavior and when refreshed material reaches only the intended nodes. No refresh/DB call belongs in the request path. A Tokenless-owned provider implementation is not required if the host adapter contract is sufficient and tested.

Acceptance: rotation and expiry during active grants, provider outage, stale refresh replies and duplicate workers fail closed without exposing credentials or expanding scope.

### T7 — SDK compatibility and conformance closure (TL-01/09/10/11)

The external consumer example and MSRV CI are delivered. Workspace packages still have `publish = false`; immutable Git dependencies are a viable initial strategy, not a blocker by themselves. Document the supported public surfaces, pinned revision/release policy and snapshot/session compatibility. A numeric snapshot version is an ordering fence, not a protocol-capability handshake. I did not find a handshake advertising strict-expiry or Git-policy semantics; reject worker attachment when required semantics are unsupported.

Extend conformance for T1–T6 and publish results for the target resource budget, saturation, shutdown, audit admission/loss and egress composition. Existing bounded connections, audit queue and shutdown reporting should be reused. Do not describe admission to an in-memory queue as durable audit persistence. The host must supply durable mutation auditing/outbox delivery where promised.

## Work still required in Temps

The current Temps PR deliberately saves intent only. `UnavailableTokenlessBroker` is still registered; provisioning and runtime revocation are unimplemented, and grant responses always set `connected: false`. No Tokenless SDK dependency is wired in. The UI/API/data-model work does not mean proxy enforcement exists.

1. **SDK adapters and policy compilation.** Pin a supported Tokenless revision; implement storage, authorization, encryption, audit and node adapters. Define a strict project-to-vault/scope mapping without introducing another user system. Translate Temps segment-boundary path prefixes correctly into Tokenless patterns. Extend the current bearer-only policy with typed Git/provider policies only when their enforcement is available.
2. **Runtime data model.** Add sandbox incarnation, assigned worker/audience, binding identity, desired/installed policy version, per-grant session/rule mapping, credential generation, lease/enforcement timestamps and safe failure states. Persist no retrievable session token in a guest-facing API. Use transactions and an outbox for desired-state writes and asynchronous delivery.
3. **Typed adapter contract.** Replace the placeholder `provision(grant_id) -> Result<()>` with an incarnation-bound, versioned command and typed acknowledgement. Separate requested, installed, ready, revoking and enforced states. Require fresh authorization/consent; old saved intents must not become live automatically when the integration is enabled.
4. **Docker and microVM transport.** Implement authenticated host-side assignment of streams to bindings, per-sandbox isolation and CA distribution. Prove that spoofed headers, shared NAT, recycled IPs, another guest or a direct egress route cannot select another binding or bypass required controls. Tokenless supplies byte-stream serving; Temps supplies Docker networking and each supported microVM transport.
5. **Lifecycle reconciliation.** Create/fence bindings on start, stop, delete, snapshot, restore and worker replacement. Revoke the old generation before making a new one usable. Remove runtime credentials before snapshots; keep keys, session tokens and CA private keys outside guest files/env/argv. Terminal-only sessions must work independently of an agent-message plugin.
6. **Credentials and policy delivery.** Implement provider issuance/refresh, node-scoped sealing, signed snapshot delivery, retries with deadlines, version fencing and event-driven reconciliation. Preserve egress policy; a service grant must not silently relax workload isolation.
7. **APIs and UI.** Expose SDK/worker capability and typed per-grant status, deadline, installation version and failure reason. Report connected only after an enforcement acknowledgement for the current generation. Add actual connect/revoke/retry actions and preserve unconfigured onboarding. Regenerate both clients.
8. **Acceptance harness.** Run Tokenless conformance against Temps adapters plus Docker and every advertised microVM backend: real Git clients, two simultaneous sandboxes, same-host grants, restart/restore, delayed messages, partitions, rotation, active expiry/revocation, secret redaction, saturation and audit failure.

Local references: `crates/temps-service-access/src/broker.rs`, `plugin.rs`, `service.rs`, `types.rs`; `crates/temps-entities/src/sandbox_access_services.rs`, `sandbox_service_grants.rs`; `web/src/components/service-access/`.

## Recommended next step

Start adapter and worker-plumbing development against the pinned SDK now, while keeping the feature in the explicit unavailable state. Agree T1–T5 before promising general time-limited access or Git support. Resolve T6 ownership and T7 compatibility/conformance as part of integration, rather than demanding Tokenless acquire Temps-specific responsibilities.

A restricted HTTPS prototype could initially use distinct-host services and host-owned refresh, but any product limitation must be enforced and shown in the UI. Do not describe that prototype as the complete feature, and do not enable even that subset without hard-expiry and revocation guarantees. No merge, release, deployment or cross-repository issue publication is requested by this assessment.
