<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-116: An ingress provider is one trait implementation

**Status:** Proposed
**Date:** 2026-11-25
**Supersedes:** [ADR-058](./058-ingress-modes-and-a-shared-app-domain.md) D3 ("tunnels are connectors, defined as data")
**Amends:** ADR-058 D8 ("providers are registered and describe themselves")

Related: ADR-064 D4 (plans, journal, undo), ADR-065 (Tailscale Serve and Services; the Temps gate and Cloudflare Access), ADR-107 (the control plane runs tunnel connectors), ADR-114 (the control plane serves private addresses).

## Context

Temps has two real ways into an app through someone else's network, and they were built two different ways.

| | Cloudflare Tunnel | Tailscale Serve / Service |
|---|---|---|
| Who reaches it | the internet | a tailnet (ADR-065) |
| Registered as | `ProviderFactory` + `TunnelProvider` (`cloudflare.rs`) | `CredentialFactory` only (`tailscale.rs`); `registry.rs` special-cases its kind in `is_credential_kind` |
| What runs on the node | a pinned `cloudflared` the agent starts (`connectors/process.rs`) | the host's own `tailscaled`, configured by the agent through its LocalAPI (`tailscale_serve.rs`) |
| Where traffic arrives | the loopback HTTP tunnel origin (ADR-107 D3), trusting `CF-Connecting-IP` | a `0700` unix socket per address (ADR-065 D3), trusting `Tailscale-User-*` |
| Names | the user's domain, CNAME | `*.ts.net`, the provider's |
| Certificates | at Cloudflare's edge | `tailscaled` gets them |

ADR-058 D8 promised a new way in would cost "one backend file". The trait it built, `TunnelProvider`, covers a connector's lifecycle (ensure, spec, publish, health, rotate, delete). But what makes a way in is the reachability engine's plan, run and undo (ADR-064 D4), and the engine never calls the trait: it names both providers directly.

- **Options:** `PlanOptionKind` has `NewTunnel` and `NewTunnelWithToken` (Cloudflare), `TailscaleServe`, `TailscaleService` and `TailscaleServiceWithCredential`.
- **Steps:** `StepKind` has `CheckToken`, `CreateTunnel`, `WaitForTunnel` and `RouteHostname` (Cloudflare), and `SaveTailscaleCredential`, `DefineTailscaleService`, `ConfigureTailscaleServe` and `WaitForServiceApproval` (Tailscale).
- **Effects:** `RunOps`, the engine's interface to the outside, mixes generic methods with tunnel methods (`create_tunnel` … `delete_tunnel`, `route_hostname`) and 13 Tailscale ones (`claim_serve_entry` … `delete_service`). Its live `create_tunnel` writes `provider: crate::cloudflare::KIND`.
- **References** (lines naming the provider, `grep -ci`):

  | File | Cloudflare | Tailscale |
  |---|---|---|
  | `reachability/plan.rs` | 129 | 299 |
  | `reachability/run.rs` | 61 | 170 |
  | `reachability/service.rs` | 42 | 213 |
  | `reachability/compute.rs` | 7 | 155 |
  | `reachability/model.rs` | 31 | 87 |

- **Pins:** `TRUSTED_ARTIFACTS` in the agent repeats the `cloudflared` builds `cloudflare.rs` pins.
- **DNS:** "who makes a name resolve" is answered twice, by `ProviderInfo.dns` (`DnsHandling`) and by `DnsIntent`.

ADR-058 D3's "operator-written connector definitions, no code path checks which is in use" was never built. What was built, a Rust provider per kind and an agent that runs only what its own build pins, is the safer design: a definition anyone with settings access can write would let them make every node run any binary.

This ADR adds no provider and changes no behaviour. It moves the two that exist behind one boundary, so the next one is a module and nothing else.

## Decision

### D1. A provider is a module the engine cannot name

Providers live in `temps-ingress/src/providers/<kind>/`, declared as private modules. The only public item is `providers::builtin() -> ProviderRegistry`. The reachability engine, the connector service, the handlers and the checks reach a provider only through the registry and the traits below, so naming `cloudflare` or `tailscale` from outside is a compile error, not a review comment.

Wire vocabulary is the one exception (D5). A `rule_drift.py` metric, `provider_variant_outside_module`, counts matches on a provider's option or step variants outside its module. Today's count is the baseline; the migration (D8) brings it to zero.

On the agent, drivers are per mechanism, not per provider: a pinned process, `tailscaled`'s LocalAPI. A connector spec names a driver and its parameters, never a provider kind.

### D2. One trait, built per connector

`TunnelProvider` and `CredentialFactory` merge into one trait, `IngressProvider`, and `is_credential_kind` goes. A factory builds an instance for one connector row, so every answer is that connector's, not the provider's in general. For example, a Tailscale machine-name connector has one host, and a Service connector may have several.

```rust
#[async_trait]
pub trait IngressProvider: Send + Sync {
    fn kind(&self) -> &'static str;

    /// What the engine must know about this connector outside its own steps.
    fn capabilities(&self) -> Capabilities;

    /// The ways this connector can serve `address`, each with the steps it
    /// takes (D3). Empty when it cannot, with the reason in `blocked`.
    async fn options(&self, ctx: &PlanContext<'_>) -> Result<ProviderOptions, IngressError>;

    /// The implementation of a step this provider planned (D3).
    fn step(&self, step: &PlannedStep) -> Result<Arc<dyn ProviderStep>, IngressError>;

    /// What a node runs or configures for this connector, if anything.
    fn node_spec(&self, node: &NodeContext<'_>) -> Option<NodeIngressSpec>;

    /// Whether `host` counts as published, given what each node reports.
    /// Replaces ADR-107 D4's "every replica serves it", which is
    /// Cloudflare's rule because Cloudflare load-balances replicas.
    fn published(&self, host: &str, nodes: &[NodeServing]) -> bool;

    async fn health(&self) -> Result<ProviderHealth, IngressError>;

    /// Invalidate the node secret and cut connections not in `keep`
    /// (today's `rotate_secret`). Default: cannot rotate.
    async fn rotate_secret(&self, _keep: &HashSet<String>) -> Result<Option<String>, IngressError> { Ok(None) }

    /// The binaries and images this connector's nodes run (D7).
    fn artifacts(&self) -> &'static [PinnedArtifact] { &[] }
}
```

The names settle in the first migration PR. The split is the decision: lifecycle and plan contribution belong to the provider, and orchestration belongs to the engine.

`Capabilities` holds only what the engine itself enforces, and only values a shipped provider uses:

```rust
pub struct Capabilities {
    /// Who can reach what it serves. The route table never gives a Private
    /// connector a public route, nor a Public one a private route.
    pub reach: Reach,              // Public | Private { network: &'static str }
    /// Where the node hands traffic to Temps, and what is trusted there (D4).
    pub origin: Origin,            // LoopbackHttp { client_address: HeaderRule, ja4: Option<HeaderRule> }
                                   // | ServeSocket { identity: &'static [&'static str] }
    /// Who makes names resolve: today's DnsHandling, now per connector.
    pub dns: DnsHandling,          // Automatic | Manual | ProviderHostname
}
```

| | Cloudflare Tunnel | Tailscale Serve / Service |
|---|---|---|
| `reach` | `Public` | `Private { network: "tailscale" }` |
| `origin` | `LoopbackHttp`, `CF-Connecting-IP`, `cf-ja4` | `ServeSocket`, `Tailscale-User-*` |
| `dns` | `Automatic` | `ProviderHostname` |

Everything else (certificates, tokens, approval waits, replicas) is behaviour: the provider's steps and `published`, not flags the engine branches on. A value is added to these enums only with the provider that needs it. `ProviderInfo` and its API shape are unchanged, and the setup form still comes from its fields.

### D3. Providers contribute steps; the engine runs them

The engine keeps the steps every way in shares: `SaveDnsProvider`, `ManageDomain`, `AttachDomain`, `SetDnsRecord`, `ShowDnsRecord`, `IssueCertificate`, `SetAudience`, `SetHttpPolicy`, `WaitForPublication` and `CheckReachability`. The rest move to their providers:

- **Cloudflare:** `CheckToken`, `CreateTunnel`, `WaitForTunnel`, `RouteHostname`, `PublishEnvironmentHosts` (ADR-068 D3), `RedirectToHttps` and `StandardPortsOnly` (Cloudflare edge rules, ADR-070 D1), and `AccessApp`. Access only ever fronts a tunnel (ADR-065 D9), so it is a Cloudflare step, not a separate kind of provider.
- **Tailscale:** `SaveTailscaleCredential`, `DefineTailscaleService`, `ConfigureTailscaleServe` and `WaitForServiceApproval`.

A step moves back into the engine when a second provider needs it. Each provider step implements:

```rust
#[async_trait]
pub trait ProviderStep: Send + Sync {
    /// What the step makes, and how a lookup finds it: immediately, or
    /// settled after two misses (ADR-064 D4, ADR-065 D4).
    fn intent(&self) -> JournalIntent;
    /// Authoritative lookup, before applying and when resuming.
    async fn find(&self, deps: &StepDeps<'_>) -> Result<Found, IngressError>;
    async fn apply(&self, deps: &StepDeps<'_>) -> Result<Applied, IngressError>;
    /// Remove only what this run made; a changed resource is a conflict.
    async fn undo(&self, deps: &StepDeps<'_>) -> Result<Undone, IngressError>;
}
```

Journal, fingerprint, conflicts and undo order stay the engine's and keep ADR-064 D4's rules. A provider cannot skip the journal, because the engine writes the entry before calling `apply`.

The provider-specific methods of `RunOps` move into the providers' `StepDeps`. `RunOps` keeps the generic ones (DNS, domains, certificates, audience, HTTP policy, environment hosts, sleep).

### D4. Where traffic arrives is the provider's declaration, and only there is anything trusted

`Capabilities.origin` replaces the two hard-coded entry points:

- `LoopbackHttp` is ADR-107 D3's tunnel origin. Only the named client-address and JA4 headers are trusted, and only on it.
- `ServeSocket` is ADR-065 D3's per-address socket. Only the named identity headers are trusted, and only on it.

Every other listener keeps stripping both sets (ADR-065 D3). A provider cannot add a trusted header on a listener it does not own, and it cannot name a new kind of origin without a new `Origin` value, which is a change to this ADR.

### D5. Saved plans, runs and the API keep their words

`reachability_plans` and `reachability_runs` store option and step kinds, and the console reads them through the API. Every `PlanOptionKind` and `StepKind` keeps its exact snake_case string, so stored plans resume, undo still runs, and the generated clients do not change.

The variants stay in `model.rs` as wire vocabulary; only their meaning moves. A golden test serializes every variant and compares it with today's strings, and changing one is refused unless it comes with a migration of the stored rows.

### D6. Checks: the token check moves, the CDN check stays

`token_check.rs` becomes Cloudflare's `CheckToken` step.

`forward_check.rs` stays where it is. Its `cf-ray` / `server: cloudflare` test recognises Cloudflare's proxy in front of an address (`Edge::Cloudflare`, reported as `cdn:cloudflare`), which an ordinary proxied DNS record produces as much as a tunnel. That is a fact about the zone's DNS provider, not about a tunnel provider, so it is not a method of the trait. A first draft of this ADR put an `edge_signature` method there; reading the check showed why it does not belong.

### D7. Pins live with the provider; the agent keeps the final say

`artifacts()` returns pinned binaries and container images by digest (Cloudflare pins both). The pins live in a dependency-free crate, `temps-connector-pins`, one module per provider, which `temps-ingress` and `temps-node-agent` both depend on. That removes the copy in `TRUSTED_ARTIFACTS` and the test keeping the copies in step.

Tailscale returns none: it uses the host's `tailscaled`, and its minimum version (1.94, ADR-065 D2) stays a check in its driver. The agent still runs only what its own build pins, so a newer control plane cannot make an older agent run a new binary.

### D8. The migration: four PRs, no behaviour change

1. **The boundary.** `IngressProvider`, `ProviderStep`, `Capabilities`, the registry merge, the golden wire test, the drift metric at baseline.
2. **Cloudflare behind it.** Its options, steps, checks and `RunOps` methods move into `providers/cloudflare/`, with `token_check.rs`, `edge_rules.rs`, `access.rs`, `access_service.rs` and `gate_check.rs`'s Access half.
3. **Tailscale behind it.** Its credential, options, the four steps, the Serve-entry code (`serve_entries.rs`) and its `RunOps` methods move into `providers/tailscale/`. `is_credential_kind` goes.
4. **Pins.** `temps-connector-pins`. Done first, with D5's golden tests, because neither depends on the rest.

**The proof** has three parts:

- the existing reachability, run and service tests pass with only imports changed, never assertions;
- the drift metric reaches zero;
- the test provider (`FakeTunnels`, `service.rs` tests) becomes an `IngressProvider` with one step of its own and `reach: Private`, and is planned, run, resumed and undone without editing any file outside it.

Each PR leaves `main` working, and the order can stop after any step.

### D9. Rules every provider keeps

- Only pinned binaries and images run (D7). Secrets reach a connector as `0600` files or environment variables, never arguments.
- Trusted headers are read only on the origin the provider declares (D4).
- `reach` is enforced by the route table: private routes never reach a public connector, and public ones never reach a private one.
- A worker runs a connector only when it delegated that ingress (ADR-058 D8, ADR-065 D2's consent flag); the control plane only as ADR-107 allows.
- **A provider's sign-in can protect an address only if Temps verifies its proof on every request, on that provider's own origin.** Today that covers two cases:
  - Cloudflare Access, where the tunnel listener verifies the `Cf-Access-Jwt-Assertion` JWT (ADR-065 D9);
  - Tailscale identity, read only on the Serve socket (ADR-065 D3, D6).

  An edge that merely says it checked someone never counts.

## Consequences

- The next provider is a module, its steps, its pins and its tests. If it needs a value `Capabilities` lacks (an origin that forwards raw TLS, an `A` record at an edge address, a certificate at Temps for a tunnelled name), it amends this ADR in the same PR and adds the value with its first user.
- This is a large refactor: the four largest files in `reachability/` (`run.rs`, `plan.rs`, `service.rs`, `compute.rs`, about 22,000 lines) change, mostly by moving code out. Splitting it into the four PRs is what keeps it reviewable.
- The engine loses its provider code: about 490 lines naming Cloudflare and 1,100 naming Tailscale across `reachability/` move into the two provider modules.
- Existing connectors, plans and runs keep working unchanged (D5).

## Not in this change

- Any new provider (Pangolin, frp, ngrok, rathole…).
- A raw-TLS origin with PROXY protocol, `A`/`AAAA` records at an edge, certificates at Temps for tunnelled names, wildcard publishing: each arrives with the provider that needs it.
- Operator-written connector definitions (ADR-058 D3's original idea).
- A provider-neutral edge sign-in: Access stays ADR-065 D9's gate, moved into the Cloudflare provider (D3) and constrained by D9 above.
- Tailscale Funnel, refused by ADR-065.
