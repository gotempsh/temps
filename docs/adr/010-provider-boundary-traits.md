# ADR-010: Provider Boundary Traits

**Status:** Accepted
**Date:** 2026-04-15
**Author:** David Viejo

## Context

Temps has two families of "pluggable backend" abstractions that every higher-level subsystem depends on:

1. **`SandboxProvider`** (in `temps-agents::sandbox`) — the boundary between "code that needs a sandboxed process to run in" (agent runs, autofixer, workspace sessions, standalone sandbox API) and "the thing that actually runs containers" (Docker, local subprocess, future Firecracker/Kubernetes backends).
2. **`WorkflowMemoryProvider`** (in `temps-memory`) — the boundary between "code that needs to load or render workflow memory" (agent executor, workspace prompt builder, future summarization job) and "the thing that actually persists facts" (Postgres-backed `WorkflowMemoryService`, in-memory fakes, future remote-cache backends).

Both traits arose organically: the Docker sandbox was first, and the trait was extracted once a second impl (local subprocess) appeared. The memory service was originally a concrete type in `temps-workspace`; the trait was extracted when the agents executor needed to call into it without creating a `temps-agents → temps-workspace` dependency cycle.

Now that both abstractions have multiple impls and multiple consumers, we need to state in writing what their contract is, who is allowed to bypass them, and how we prevent regressions.

## Decision

### 1. Each domain has exactly one provider trait

There is **one** `SandboxProvider` trait, in `temps-agents::sandbox`. There is **one** `WorkflowMemoryProvider` trait, in `temps-memory`. Adding a new backend means adding a new `impl` of the existing trait, never a new trait.

Compile-time assertions pin each trait as object-safe — every consumer holds `Arc<dyn ProviderTrait>`, and breaking object-safety (generic methods, `Self` return types, associated constants requiring a concrete type) cascades through every caller. The tests are:

- `temps-agents/src/sandbox/mod.rs :: compile_asserts_object_safety`
- `temps-memory/tests/eval.rs` (in-memory reference implementation: trait must be implementable)

### 2. Consumers hold trait objects, not concrete types

Every subsystem that *uses* a provider holds `Arc<dyn SandboxProvider>` or `Arc<dyn WorkflowMemoryProvider>`. This is non-negotiable — it is the point of the boundary. Concrete types (`DockerSandboxProvider`, `WorkflowMemoryService`) may only be held by:

- The plugin that **constructs** the provider (to wire its dependencies).
- The HTTP handler layer for the same crate that owns the concrete type, when it needs methods outside the trait surface (e.g. admin CRUD on memory facts not exposed through the `WorkflowMemoryProvider` trait).

The agents executor, workspace message builder, autofixer, and standalone sandbox API all hold `Arc<dyn Trait>`. None of them know whether the provider is backed by Docker, Postgres, or an in-memory test fake.

### 3. Eval harnesses pin the trait contract

The trait contract is expressed as an **evergreen eval harness** the reference impl runs against in CI. Any new backend must pass the same harness before being accepted. The harnesses are:

- `temps-agents/tests/` for `SandboxProvider` (Docker-gated — skips cleanly when Docker is unavailable).
- `temps-memory/tests/eval.rs` for `WorkflowMemoryProvider` (pure in-memory reference impl — always runs).

A PR that changes the semantic contract (e.g. "load_for_trigger must filter superseded facts") must update the harness in the same PR, making the semantic change visible in review.

### 4. What may bypass the boundary

The only legitimate reasons to call a backend directly are:

- **Construction**: the plugin that owns the backend can import `DockerSandboxProvider::new(...)` to create it.
- **Admin CRUD**: the HTTP handler in `temps-workspace` may call `WorkflowMemoryService::delete_fact` directly because the trait deliberately omits hard-delete (it's a compaction-only operation). Any *other* handler that reaches for the concrete type is a smell — lift the missing method onto the trait and add it to the eval harness.

Everything else — executors, autofixers, session managers, standalone sandbox APIs — must go through the trait.

### 5. CI guard

A lightweight grep-based guard runs in CI to catch new violations. It fails the build when a consumer crate (any crate *other* than the one that constructs the backend) imports the concrete type directly. See `scripts/check-provider-boundary.sh` — it's fast enough to run in the pre-commit hook and in CI, and specific enough that false positives are rare.

## Consequences

### Positive

- **Swap-in safety.** Any new backend that passes the eval harness is a drop-in replacement. PR 3.4 demonstrates this: we wire the in-memory `WorkflowMemoryProvider` into the prompt-builder tests, proving the consumer is genuinely trait-scoped.
- **No cycles.** The `temps-agents → temps-workspace` dependency cycle that would otherwise form (agents executor needs memory, memory lives in workspace) is broken by the trait living in `temps-memory` — a leaf crate that both depend on.
- **Docs as contract.** The trait rustdoc + eval harness together form the provider contract. Breaking it is a reviewable act, not an accidental one.

### Negative

- **Slight indirection.** `Arc<dyn Trait>` is marginally slower than a monomorphized `Arc<Concrete>`. For the workloads these traits cover (container lifecycle, memory row loads — both already async + IO-bound), the overhead is lost in the noise.
- **Abstract-method design cost.** Adding a new method to the trait means implementing it on every backend, not just the one that needs it. We mitigate this with **default method impls that return "not supported"** for write-shaped methods on the memory trait — lightweight consumers (read-only caches, test fakes) don't have to stub them.
- **Reviewer vigilance.** The CI guard is grep-based, which is imperfect. A determined dev can work around it by renaming types or going through a re-export. This is acceptable — the point is to catch the *accidental* boundary break, not to defend against adversarial commits.

## Alternatives considered

- **Leave the abstraction implicit.** Rejected: we already paid the cost of extracting the traits. Documenting the policy and adding a CI guard is the cheap final step that makes the abstraction durable.
- **Move both traits into `temps-core`.** Rejected: `temps-core` is already too large, and the sandbox trait pulls in enough type surface (`SandboxHandle`, `SandboxCreateConfig`, `KillSignal`, `ExecStream`, …) that moving it would bloat `temps-core`'s public API significantly for no win. The current location (`temps-agents::sandbox` and `temps-memory`) keeps the trait near its natural primary consumer.
- **Procedural-macro CI guard** (e.g. a custom lint). Rejected for now: the grep guard is ~20 lines, runs in milliseconds, and handles the 99% case. We can upgrade to a real lint (Dylint, cargo-deny, or a clippy contribution) if the grep guard produces false positives in practice.

## Scope

This ADR applies to provider-shaped abstractions where multiple backends are genuinely expected. It does **not** extend to:

- Single-backend services (e.g. `EncryptionService`) — no boundary, no trait needed.
- Internal-only helpers (e.g. `TriggerContext`, `WriteFactRequest`) — these are shared data shapes, not provider boundaries.
- HTTP-surface stability — that's ADR-009's domain.
