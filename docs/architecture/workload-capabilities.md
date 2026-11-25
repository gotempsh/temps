<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Workload capabilities: write it once, for every kind

How to add something that a sandbox, an environment's deployments, a managed
service, a function or a job could all have, without building it once per
product. The decision behind it is
[ADR-115](../adr/115-workload-capabilities-written-once.md); the workload
model is [ADR-052](../adr/052-common-workload-model.md).

## The rule

**Different entities and different data are fine. Duplicated functionality
is not.**

- A **product** (sandboxes, projects and their environments, managed
  services) owns its tables, its lifecycle, its permissions, its URLs and its
  pages. Nothing asks two products to share a table or become one concept.
- A **capability** of the things those products run (record outbound
  connections, send telemetry, apply a network policy, show activity) is
  implemented once, against the workload identity, in a workload-level crate.
- A product connects to a capability through a small **adapter** that
  answers only what differs for it.

If you are about to write, in a product crate, logic that would read the same
for another product with the table name changed, stop: it belongs in a
workload crate behind an adapter.

## Is it a capability?

Ask whether the feature would make sense for another kind of workload. If a
deployed service could want it as much as a sandbox does, it is a capability.

| Capability (shared) | Product concern (stays with the kind) |
|---|---|
| The flow: validate, decide, store, read | Which row holds the setting |
| Ingest, attribution, retention | Who owns the workload, and who may see it |
| Record and response shapes, errors, the audit entry | The URL and the permission that guards it |
| Background loops and their bounds | Where it runs, and how its traffic or data gets there |
| The console component | The page it is on, and its noun |

## The identity

Everything shared is keyed by `temps_workload::WorkloadRef { kind, id }`:

| Kind | `id` is | Example |
|---|---|---|
| `sandbox` | the sandbox's row id | `sandbox/42` |
| `deployment` | the **environment's** id (not a deployment row's) | `deployment/7` |
| `service` | the managed service's id | `service/3` |
| `function`, `job` | their own ids | |

Never key shared code by a product id (`sandbox_id`, `project_id`) or by an
address. Addresses appear in one place, `IdentityMap`.

## Where code goes

| Crate | What belongs there |
|---|---|
| `temps-workload` | Leaf types only: identity, spec, labels. No database, no other Temps crate. |
| `temps-workload-netpol` | The per-node proxy and enforcer, policy and record types. Runs on nodes; no database. |
| `temps-workload-egress` | Recording outbound connections: the flow, ingest, activity, the node-0 recorder, steering, shared API types. The worked example. |
| `temps-workload-plane`, `-env`, `-volumes`, `-ingress`, `-docker`, `-node`, `-activator` | The workload plane's other concerns. |
| a product crate (`temps-sandbox`, `temps-deployments`, `temps-environments`, ...) | Its entities, its authorization, its handlers, its adapter. |

A new capability gets its own `temps-workload-<name>` crate (or joins the one
it extends). Give it a one-line `description`, add it to `LAYERS` in
`scripts/crate_map.py`, and register its plugin after the plugins whose
services it builds on.

## The recipe

`temps-workload-egress` is the reference. Read it next to the two adapters:
`crates/temps-sandbox/src/services/egress_recording/` and
`crates/temps-deployments/src/services/egress_recording/`.

### 1. The shared service

- Takes a resolved workload (`RecordedWorkload` there), never a product id
  and never a user: the caller has already resolved and authorized.
- Owns the typed error enum, with the workload's noun and id in every
  message, and its `From<..> for Problem`.
- Owns the request, response and audit types, so they are identical for
  every kind.
- Holds one adapter per kind in a registry and fails with a typed error for
  a kind that registered none.

### 2. The adapter trait

Keep it to the questions that differ. For recording they are: the kind, its
noun, the attribute that names it on a record, how to load workloads by id,
where to store the opt-in, and what to do on this host when it changes.

A new method on the trait needs a reason that is true of every kind. A
method only one kind would implement is a sign the logic belongs in that
product.

### 3. Storage

- Configuration is a column on the row it configures, in the product's own
  table. The adapter reads and writes it.
- Data the capability produces is owned by the workload: store the workload
  kind and id, index by them, and make the project optional. A workload
  without a project must still work.

### 4. The product side

Each product adds three thin pieces:

1. **An adapter** implementing the trait from its own tables.
2. **A facade** that resolves the caller's workload in the product's terms
   (the sandbox's owner; an environment inside the project in the URL) and
   calls the shared service. Not found for anything the caller may not see.
3. **Handlers** at the product's own URLs with the product's own guards
   (`permission_guard!`, the project-scope guards), using the shared request
   and response types and the shared audit entry.

In the plugin's second phase, register the adapter with the shared service.
Declare the shared service in `required_services()`.

### 5. Whatever really differs

Put it behind a second, equally small trait and say in the capability's
documentation why it differs. For recording it is how traffic reaches the
proxy (`LocalTraffic`): a sandbox's sidecar hands traffic over, a deployment
container is started with proxy variables. The proxy, the table and the
records are still one.

### 6. The console

One component per capability under `web/src/components/workloads/`, taking a
workload descriptor: its noun and the functions that call its endpoints.
Product pages render that component; they do not copy it.

### 7. Tests

- The flow is tested once, in the shared crate, against a fake kind that
  exists only in the tests. If a shared test needs a real product's table,
  the logic is in the wrong place.
- Each product tests its adapter against a real database, its authorization,
  and one path through the shared service end to end.

## Review checklist

- [ ] Shared logic takes a `WorkloadRef`, not a product id.
- [ ] No second copy of a flow, ingest, query, response type or audit entry.
- [ ] The opt-in or setting is a column on the product's own row.
- [ ] Produced data is stored and read by workload kind and id, and works
      without a project.
- [ ] Handlers are in the product, guarded in the product's terms, and call
      the shared service.
- [ ] A kind that did not register is refused with a typed error, not
      ignored.
- [ ] The console surface is the shared component.
- [ ] What differs by kind is behind a trait, and the documentation says why
      it differs.

## When not to share

- The feature only makes sense for one product (a sandbox's snapshots, an
  environment's custom domains). Keep it in the product.
- Two features look alike but are decided by different rules. Sharing the
  storage and the response shape may still be right; sharing the decision is
  not.

Sharing early is cheaper than merging later: the second kind is when a
product-shaped feature should move, not the third.
