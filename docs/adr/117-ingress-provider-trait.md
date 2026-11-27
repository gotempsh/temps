<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-117: An ingress provider is one trait implementation

**Status:** Proposed
**Date:** 2026-11-25
**Supersedes:** [ADR-058](./058-ingress-modes-and-a-shared-app-domain.md) D3 ("tunnels are connectors, defined as data")
**Amends:** ADR-058 D8 ("providers are registered and describe themselves")

Related: ADR-064 D4 (plans, journal, undo), ADR-065 (Tailscale Serve and Services; the Temps gate and Cloudflare Access), ADR-107 (the control plane runs tunnel connectors), ADR-115 (the control plane serves private addresses).

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

Providers live in `temps-ingress/src/providers/<kind>/`, declared private to `providers` outside tests (tests reach in for fixtures). Outside it the crate sees two items:

- `providers::builtin()`, the factories the registry is built from;
- `providers::Tailnet`, the state Tailscale's planning keeps (see "Provider state the engine holds" below).

The reachability engine, the connector service, the handlers and the checks reach a provider only through the registry, the traits below and that handle. Naming `cloudflare` or `tailscale` from outside is a compile error, not a review comment.

A `rule_drift.py` metric, `provider_named_outside_module`, counted the moves down. It counts every `cloudflare::` or `tailscale::` path, and every `"cloudflare_tunnel"` or `"tailscale"` literal, in `temps-ingress` production code outside `providers/`. It started at 35 and is now 0. With no baseline entry, any new crossing fails the check, which also catches a test-only path that the compiler allows. Wire vocabulary (D5) is the one exception, because stored step and option names keep their place in `model.rs`.

On the agent, drivers are per mechanism, not per provider: a pinned process, `tailscaled`'s LocalAPI. A connector spec names a driver and its parameters, never a provider kind.

### D2. One registry, answering per connector

`CredentialFactory` and the special case for its kind (`is_credential_kind`, which named `"tailscale"`) are gone. Every provider registers one `ProviderFactory`, and the registry tells tunnels from credentials by asking the provider, not by its name:

```rust
#[async_trait]
pub trait ProviderFactory: Send + Sync {
    fn info(&self) -> ProviderInfo;

    /// What this provider's way in is like: for `connector` when there is
    /// one, else for a way in that needs no connector row (a Tailscale
    /// machine name).
    fn capabilities(&self, connector: Option<&ingress_connectors::Model>) -> Capabilities;

    async fn validate(&self, deps: &ProviderDeps<'_>, setup: &ProviderSetup) -> Result<(), IngressError>;

    /// The tunnel side, for a provider whose connector rows are tunnels.
    /// `None` for one whose rows are credentials (Tailscale's OAuth client).
    fn tunnels(&self) -> Option<&dyn TunnelFactory> { None }
}

#[async_trait]
pub trait TunnelFactory: Send + Sync {
    /// The tunnel for one stored connector, with its credentials.
    async fn build(&self, deps: &ProviderDeps<'_>, connector: &ingress_connectors::Model,
                   secrets: &BTreeMap<String, String>) -> Result<Box<dyn TunnelProvider>, IngressError>;
    fn spec_provider(&self, http: &reqwest::Client) -> Box<dyn TunnelProvider>;
}
```

`TunnelProvider` keeps its name and its lifecycle methods (ensure, connector spec, publish, health, rotate, delete): it is the tunnel side, built per connector, and only tunnels have one. Capabilities are answered per connector, as decided, with a connector-less answer for ways in that have no row: a Tailscale machine name needs no credential and so no connector row, which is why "per connector" could not mean "only for a row".

The plan and run contribution (D3) joined `ProviderFactory` as two optional hooks:

```rust
    /// Its part of planning, for a provider that plans audiences whole.
    fn plans(&self) -> Option<&dyn ProviderPlan> { None }
    /// Its part of one run, over the live effects.
    fn run<'a>(&self, live: &'a LiveOps) -> Option<Box<dyn ProviderRun + 'a>> { None }
```

`Capabilities` holds only what the engine itself enforces, and only values a shipped provider uses:

```rust
pub struct Capabilities {
    /// Who can reach what it serves. The route table never gives a Private
    /// connector a public route, nor a Public one a private route.
    pub reach: Reach,              // Public | Private { network: &'static str }
    /// Where the node hands traffic to Temps, and what is trusted there (D4).
    pub origin: Origin,            // LoopbackHttp { client_address_header, ja4: Option<Ja4Header> }
                                   // | ServeSocket { identity_headers }
    /// Who makes names resolve: today's DnsHandling, now per connector.
    pub dns: DnsHandling,          // Automatic | Manual | ProviderHostname
}
```

| | Cloudflare Tunnel | Tailscale Serve / Service |
|---|---|---|
| `reach` | `Public` | `Private { network: "tailscale" }` |
| `origin` | `LoopbackHttp`, `CF-Connecting-IP`, `cf-ja4` | `ServeSocket`, `Tailscale-User-*` |
| `dns` | `Automatic` | `ProviderHostname` |

Two consumers read them already: a Cloudflare connector's spec takes its trusted headers from its declared origin (a test checks every tunnel provider's spec against its origin), and the certificate check skips a tunnel whose `reach` is private instead of one named `tailscale`. Everything else (certificates, tokens, approval waits, replicas) is behaviour: the provider's steps, not flags the engine branches on. A value is added to these enums only with the provider that needs it. `ProviderInfo` and its API shape are unchanged, and the setup form still comes from its fields.

### D3. Providers contribute steps; the engine runs them

The engine keeps the steps every way in shares, and the steps every tunnel shares: `SaveDnsProvider`, `ManageDomain`, `AttachDomain`, `SetDnsRecord`, `ShowDnsRecord`, `IssueCertificate`, `SetAudience`, `SetHttpPolicy`, `WaitForPublication`, `CheckReachability`, and `WaitForTunnel`, `RouteHostname` and `PublishEnvironmentHosts` (ADR-068 D3). The tunnel steps act on Temps' own connector rows through the connector service, whose provider-specific half is already the `TunnelProvider` trait (ADR-058), so a second tunnel provider shares them as they are. The rest move to their providers:

- **Cloudflare:** `CheckToken` (its API token), `CreateTunnel` (a Cloudflare connector on a Cloudflare account), `RedirectToHttps` and `StandardPortsOnly` (Cloudflare edge rules, ADR-070 D1), and `AccessApp`. Access only ever fronts a tunnel (ADR-065 D9), so it is a Cloudflare step, not a separate kind of provider.
- **Tailscale:** `SaveTailscaleCredential`, `DefineTailscaleService`, `ConfigureTailscaleServe` and `WaitForServiceApproval`.

A step moves back into the engine when a second provider needs it. A provider contributes its steps for one run, and finds and undoes what they journaled:

```rust
#[async_trait]
pub trait ProviderRun: Send + Sync {
    fn runs(&self, kind: StepKind) -> bool;
    async fn step(&self, run: &mut dyn StepRun, kind: StepKind) -> Outcome;
    /// The journal intents and resources this provider finds and removes.
    fn owns(&self, intent: &Intent) -> bool;
    fn owns_resource(&self, resource: &RunResource) -> bool;
    /// Authoritative lookup, when resolving a pending entry.
    async fn look(&self, run_id: i32, intent: &Intent) -> Result<Found, String>;
    /// Before removing: still what the run made, gone, no longer the run's,
    /// changed since (undo pauses), or unreadable (undo stops).
    async fn check_undo(&self, resource: &RunResource, checked: bool) -> UndoCheck;
    async fn remove(&self, resource: &RunResource) -> Result<(), String>;
}
```

A step reaches the run only through `StepRun`, which the engine implements: the run's input, `begin` before a creating call, `finish` or `finish_failed` after it, and the few facts steps hand each other (the DNS provider saved, the tunnel made, the gate in front of the address). Journal, fingerprint, conflicts and undo order stay the engine's and keep ADR-064 D4's rules. A provider cannot skip the journal: `begin` stores the pending entry, and refuses, before the provider's call.

The provider-specific methods of `RunOps` move into each provider's own ops trait (`CloudflareOps`, `TailscaleOps`), which the live ops and the test fake both implement. `RunOps` keeps the generic ones (DNS, domains, certificates, connectors, hostname choices, audience, HTTP policy, environment hosts, sleep), and `RunOps::providers()` lists the providers of a run from the registry.

**Planning.** A provider that plans an audience whole contributes a `ProviderPlan`, and the engine asks `ProviderRegistry::planner(audience)` before falling back to its own plan:

```rust
pub trait ProviderPlan: Send + Sync {
    fn plans(&self, audience: Audience) -> bool;
    /// The address it would suggest, or why it cannot.
    fn address(&self, facts: &PlanFacts) -> Result<Option<String>, String>;
    fn plan(&self, facts: &PlanFacts) -> ReachabilityPlan;
    /// The plan `choices` shows, when it differs from `plan`.
    fn choice_plan(&self, facts: &PlanFacts) -> ReachabilityPlan { self.plan(facts) }
}
```

Tailscale plans `Private` and `TailnetUsers`. With no planner, `Private` keeps the engine's own listener plan, and `TailnetUsers` says Temps cannot plan an address for it.

**Provider state the engine holds.** Planning and applying ask the tailnet questions before any run exists: they read the cluster's OAuth client and prove its scopes, read the Service an address names and its hosts, check that the hosting worker is a device of the credential's tailnet, and take a pasted credential. That code, and its short-lived caches, is Tailscale's: `providers/tailscale/service.rs` defines `Tailnet`, the engine holds one, and it calls the handle's methods without knowing how they work. A run's own reachability check gets `Tailnet::uncached()`, so a check made after the run's changes never reuses a read made before them.

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

A provider's pinned binaries and container images (by digest; Cloudflare pins both) live in a dependency-free crate, `temps-connector-pins`, one module per provider, which `temps-ingress` and `temps-node-agent` both depend on. The provider builds its node spec from them, and the agent trusts exactly `temps_connector_pins::binaries()`. That removed the copy in the agent's `TRUSTED_ARTIFACTS` and the test that parsed `cloudflare.rs` to keep the copies in step.

Tailscale pins nothing: it uses the host's `tailscaled`, and its minimum version (1.94, ADR-065 D2) stays a check in its driver. The agent still runs only what its own build pins, so a newer control plane cannot make an older agent run a new binary.

### D8. The migration: four PRs, no behaviour change

1. **The boundary.** `Capabilities`, the registry merge (D2), the golden wire test (D5), the drift metric at baseline (D1). Done.
2. **Cloudflare behind it.** Its steps, `RunOps` methods and edge-rule calls moved into `providers/cloudflare/`. Done for everything that named it. Still under `reachability/`, with no provider path but Cloudflare-only in what they do: `token_check.rs`, the rest of `edge_rules.rs`, `access.rs`, `access_service.rs` and `gate_check.rs`'s Access half.
3. **Tailscale behind it.** Its credential, plan, the four steps, its `RunOps` methods and its planning state (`Tailnet`) moved into `providers/tailscale/`. `is_credential_kind` is gone. Still under `reachability/`: `serve_entries.rs`, the Serve-entry rows the agent reads, and apply's mapping from a Tailscale option to the run's input (see the proof below).
4. **Pins.** `temps-connector-pins`. Done first, with D5's golden tests, because neither depends on the rest.

**The proof** has three parts:

- the existing reachability, run and service tests pass with only imports changed, never assertions;
- the drift metric reaches zero;
- the test provider (`FakeTunnels`, `service.rs` tests) becomes a provider with one step of its own and `reach: Private`, and is planned, run, resumed and undone without editing any file outside it.

**Where the proof stands.** The first part holds: no assertion changed. Two tests changed their fixture, never their expectation: the DNS-retry test now sets the connector's DNS capability instead of renaming its provider, and the Service-label test calls the one label check that is left. The second part holds: the metric is 0 and the modules are private.

The third part cannot be met as written, because it conflicts with D5. A step of a provider's own needs a `StepKind` word, its option a `PlanOptionKind` word, and what it journals an `Intent` and a `RunResource`. D5 keeps all four as closed wire vocabulary in `model.rs`, so that stored plans and runs keep their strings. A new provider therefore edits `model.rs`. Apply also maps each option kind to the run's input (`RunInput`) in `service.rs`, so a new option needs a branch there too. Borrowing an existing word for the test provider would make the proof pass without proving anything. Two ways forward:

- **Narrow the proof.** A new provider is its module plus its words in `model.rs`. Meeting that needs one more move: a provider hook that maps its own options to the run's input, which also takes apply's Tailscale branches out of the engine.
- **Open the vocabulary.** Add a provider-named variant to the four enums (`provider:<kind>:<word>`). Stored rows are unaffected, but this is a wire change: the generated clients gain a variant and the golden test grows. That is beyond this refactor.

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
