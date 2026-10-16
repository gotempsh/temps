<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Multi-node deployments and build dispatch

- **Status:** Proposed. Part of #30 and #1528. This document is a design and a gap
  analysis; the only code that accompanies it is the verification contract in
  `crates/temps-deployer/src/build_verification.rs`, which no deployment path calls yet.
- **Builds on:** ADR-017 (split proxy/console), ADR-020 (multi-node hardening),
  ADR-048 (enrollment hubs and ingress), the worker build protocol (#1146), the
  per-environment build location (#1321).
- **Does not overlap:** the control-plane and ingress HA proposal (#1048), and the
  scan and image-config work on builder nodes (#1545, for #1527). Where this
  document depends on either, it says so.

## 1. Why one document

#30 asks for workloads and managed services to be placed on several machines. #1528
asks for builds to run in CI-provider compute. They look unrelated, and they share
one mechanism: **the control plane hands work to a machine it does not run, and has
to decide how far to trust what comes back.** A worker node is the trusted case; a
CI runner is the untrusted one. Designing them separately would produce two
protocols, two identities and two fallback rules. This document keeps one of each.

## 2. #30: what exists, what is missing

#30 was written when `temps-agent`, `temps-wireguard` and the node scheduler were a
branch. They are in the tree, so most of the issue is delivered. The table maps each
of its bullets to the code that answers it; "missing" means no code, not "unpolished".

| #30 asks for | State | Where |
|---|---|---|
| Agent binary on worker nodes | Delivered | `crates/temps-agent`, `temps agent` |
| Node registry with address, status, capacity, health | Delivered | `nodes` entity, `crates/temps-deployments/src/services/node_service.rs`, heartbeat at `/internal/nodes/{node_id}/heartbeat` |
| Scheduler: resource availability, affinity, manual assignment | Delivered; least-loaded plus anti-affinity | `node_scheduler.rs`; `configured_target_nodes` / `target_labels` in `DeploymentConfig` |
| WireGuard mesh between control plane and workers | Delivered | `crates/temps-wireguard`, `crates/temps-network`, ADR-048 |
| `node_id` on containers and managed services | Delivered | `deployment_containers`, `external_services` entities |
| Deployer routes container operations to the right agent | Delivered | `RemoteNodeDeployer`, `crates/temps-deployer/src/remote.rs` |
| `temps join`, heartbeat, drain, remove | Delivered | `crates/temps-cli/src/commands/join.rs`, `node.rs`; drain rebuilds elsewhere (see `docs/features/multi-node/page.mdx`, *Current limitations*) |
| Node settings page, CRUD and capacity API | Delivered | `web/src/pages/settings/NodesPage.tsx`, `handlers/nodes.rs` |
| Deployment creation can target nodes | Delivered, per project and environment | `deployment_config.configured_target_nodes`, `target_labels` |
| Managed services deployable on any node | Delivered for the services that run in a container; HA database topology is ADR-011 | `crates/temps-providers`, `service_handlers.rs` in the agent |
| **No single point of failure on the control-plane host** | **Missing** | proposed in #1048 |
| **Bin-packing across CPU, memory and disk** | **Missing** (placement is least-loaded) | `node_scheduler.rs` |
| **Sandboxes that move, or run with a full feature set, on workers** | **Missing** | feature page, *Sandboxes on worker nodes* |

Consequences for the issue:

1. **#30 is an umbrella that is mostly closed.** It is left open, not closed, because the
   "no high availability" bullet of its own problem statement is not met: today, losing the
   control-plane host loses ingress and database role discovery even when workloads survive.
   That work has a proposal (#1048) and failure-injection acceptance criteria. #30 should
   close when that ADR is accepted *and* implemented, or be closed with the remainder split
   into issues of their own.
2. **Nothing in this document re-designs HA.** Where build dispatch has an HA implication
   (a build in flight when a console dies), section 7 states the interface it needs from
   #1048 rather than inventing one.
3. **Builds are the one placement decision that was missing a first-class model**, which is
   what the rest of this document is about.

## 3. Builds today (the #718 baseline)

Builds can already leave the control plane, through the agent:

```
BuildLocation (per project, overridden per environment)  -- deployment_config.rs
        |
        v
select_offload_builder / select_node_builder             -- workflow_execution_service.rs
        |   prefers a node labelled temps.sh/role=builder of the right architecture;
        |   otherwise the node the deployment is placed on; else the control plane
        v
RemoteNodeDeployer: ImageBuilder over HTTP               -- remote.rs, POST /agent/images/build
        |   multipart: JSON BuildSpec (protocol v1, v2 with build args) + streamed context tar
        |   response: newline-delimited BuildEvent { Log | Result | Failure }
        v
the image stays on the builder; replicas on other nodes receive it from there
```

Properties that the dispatch design keeps rather than replaces:

- **The seam is `ImageBuilder`.** `WorkflowPlanner`, `BuildImageJob`, the pipeline view and
  the log viewer are untouched by where a build ran. `BuildLocation::Node` is already "a
  preference, not a requirement": no eligible worker means a local build and a log line
  saying why.
- **The wire contract is versioned and bounded** (`build_protocol.rs`): spec size, context
  size and entry count, build-arg count and bytes, path-traversal checks, `BuildFailureKind`.
- **Secrets stay home.** A worker build receives only non-secret variable values as
  `--build-arg`; secret variables, linked-service credentials, the deployment token and
  registry credentials are never sent, and a Dockerfile that would read one is refused by
  name before upload.
- **Static sites** build on any worker and the control plane extracts only the output.

What a builder node cannot do is serve a machine the control plane has no relationship
with. That is the entire gap #1528 fills.

## 4. #1528: dispatching builds to CI providers

### 4.1 Position

A CI runner is **another executor behind `ImageBuilder`, speaking the build protocol that
already exists**. It is not a second pipeline, a `HostedBuildJob`, or a separate artifact
with its own release train. #1528 itself asks for this ("ship the runner as a subcommand ...
reuse the agent build protocol"), and it is the only shape under which "a remote build
behaves like a local one" stays a property of the types instead of a promise.

The executor ladder, ranked by how much of a third party each rung needs:

| Rung | Executor | Needs | State |
|---|---|---|---|
| L0 | A Temps worker node, `temps.sh/role=builder` | nothing external | **Delivered** (#1146, #1321, #1299, #1372, #1402, #1418) |
| L1 | A pull-mode runner on any machine | outbound HTTPS | Proposed, phase 1 |
| L2 | Dispatch to GitHub Actions, GitLab CI, Gitea Actions, a k8s Job | a CI API | Proposed, phase 2 |

Each rung is opt-in per environment; local stays the default; every rung can fall back to
the one before it only for the failures section 4.6 lists.

### 4.2 The runner

`temps build-runner`, a subcommand of the existing binary (and the same binary in a slim
image). It performs the work `temps-agent` already performs for `/agent/images/build`,
with the transport inverted:

```
authenticate -> GET spec -> GET context -> build -> report digest -> POST result
```

What is reused versus new:

| Piece | Reused | New |
|---|---|---|
| `BuildSpec`, limits, path checks, `BuildEvent` | `build_protocol.rs` unchanged | none |
| Build execution (BuildKit, cache namespace, build args) | the agent's build handler | extraction into a function both callers use |
| How the spec and context arrive | multipart push on the agent channel | pull: `GET .../spec`, `GET .../context` with a build-scoped credential |
| Log return | the NDJSON `BuildEvent` stream | the same events posted to `POST .../logs` |
| Result | `BuildResult` | a digest-bearing report (section 4.4) |

The runner needs no Docker socket (a socket mount is root-equivalent on a machine that
may run other things). It declares its backend, platforms and isolation as capabilities.

### 4.3 Identity: configuration, not code

Trust anchors are rows, so adding a CI system is an admin action rather than a release.
This follows the repo rule that runtime configuration is a column on an entity and not an
environment variable.

```text
trusted_identity_issuers
  id, name, issuer_url, jwks_uri?, audience,
  required_claims (jsonb), run_id_claim,
  enabled, created_at, updated_at
```

Four fields describe both GitHub Actions (`repository`, `run_id`) and GitLab CI
(`project_path`, `namespace_path`, `pipeline_id`, `job_id`). Below OIDC, an enrollment
token modelled on `node_enrollment_tokens` serves pull-mode runners and CI without OIDC;
for L0, mTLS over the cluster CA is what exists today.

Every anchor produces the same artifact: a `deployment_tokens` row valid for **one build**,
carrying `deployment_id`, a short `expires_at` and four permissions (`builds:read`,
`builds:logs`, `builds:artifacts`, `builds:result`), revoked on first result. A
compromised runner leaks one build's context, which it already had on disk.

The nearest precedent for discovery and JWKS handling is `temps-auth`'s `oidc_service.rs`
(SSO `id_token` verification, with a JWKS refresh-and-retry on key rotation). It is built
around an interactive client flow, not a bearer token checked against an audience and
required claims, so phase 2 decides whether to extend it or share its JWKS cache. What it
must not do is introduce a second, independent JWKS cache. **Not implemented in this
increment** (section 8).

### 4.4 The result, and what the control plane believes

A runner reports a **digest**, never an image reference.

```
dispatched (control plane records, before handing off)
    build_id, deployment_id, run_id (provider handle), platforms, image_repository
runner reports
    build_id, run_id, digest, platforms
control plane observes, independently, and says what kind of identifier it is
    RegistryDigest    a manifest digest resolved from a registry
    ImportedImageId   the id the local daemon assigned on tar import
```

The two are not interchangeable. A tar-imported image has an image id (the digest of its
config) and no manifest digest, so `<repository>@<image id>` resolves against nothing. The
reference is therefore built server-side from the kind that was observed:
`<image_repository>@<digest>` for a registry digest, from a repository the control plane
chose before dispatch, and the image id itself for a tar import. Verification
(`build_verification::verify_runner_report`) refuses, with an error that names the
deployment and build:

- a different build id, or a run handle that differs from the recorded one (including a
  run the control plane never recorded, or a recorded run the report omits);
- a digest that is not `sha256:` plus 64 lowercase hex;
- a reported digest that is not the digest the control plane observed;
- a platform set that is not exactly the requested set, in either direction, and a
  platform list longer than 16 entries or with an entry over 64 bytes (the count is
  checked first and rejected before any work proportional to the list, so the diagnostic
  is bounded too);
- a dispatch record whose repository carries a tag or digest, or whose registry host or
  any path component does not follow the distribution grammar (so `team:tag/app`, `..`
  and `-app` are refused rather than yielding an undeployable reference).

Runner-supplied strings are bounded and escaped before they reach an error, because those
errors end up in build logs.

### 4.5 Artifact return

| Sink | When | Cost |
|---|---|---|
| Local | L0: build on the node that will run it | nothing moves |
| Tar upload (`deploy/image-upload`) | default for L1/L2 | no credentials; the whole image crosses the control plane, existing 1 GB cap |
| OCI registry | the operator supplies a credential | layer dedup, manifest lists, nodes pull directly (needs an authenticated pull on the agent) |

Tar is the default because a registry sink cannot be made zero-configuration on every
provider: GHCR rejects GitHub App installation tokens for pulls, so a registry needs an
operator-supplied credential, stored encrypted on its row via `EncryptionService`.

**Dependency on #1545.** The result of a remote build also has to carry the image
configuration (exposed ports, user, entrypoint, working dir) so `DeployImageJob` and the
asset jobs do not need the image on the control plane. #1545 introduces
`ImageInfo.config: Option<ImageConfig>` for exactly this on builder nodes. The remote
result must reuse that type, not define a parallel one, so phase 1 is sequenced after
#1545 merges. The scan likewise moves to wherever the image is (#1545 does it for nodes;
a runner would do it in-run, optionally, since today the scan is non-blocking).

### 4.6 Failure taxonomy and fallback

`RemoteBuildFailure::disposition` encodes the rule once, so no executor reimplements it:

| Failure | `fallback_to_local` on | `fallback_to_local` off |
|---|---|---|
| Not configured, no eligible runner, queue timeout, capability mismatch, lease lost, dispatch failed (incl. shim missing) | Warn in the build log, build locally | Fail the deployment |
| **Report fails verification** (digest, platform, build or run mismatch) | **Fail the deployment** | **Fail the deployment** |
| **Runner identity rejected** | **Fail the deployment** | **Fail the deployment** |
| Runner reports its own build as failed | Fail the deployment | Fail the deployment |

The last row is deliberate: a Dockerfile error is not missing infrastructure. Rebuilding
the same source on the box the operator was protecting would hide it behind a slower
deploy and double the cost.

### 4.7 Cancel and restart

A remote build is long-lived and can outlive a console restart. The run handle is
persisted into `deployment_jobs.outputs` on acquire, before the await, and a reconciler
re-attaches to in-flight builds at startup through a queue job shaped like
`DeploymentGateRecheck` (`job_processor.rs`). Cancelling a deployment cancels the provider
run. A restart with no handle and no result fails the deployment with that reason; it
does not guess.

Under #1048's multiple consoles, the reconciler must run under that ADR's job claim and
fencing token, so two consoles do not both re-attach to, or both cancel, one run. This is
the one interface this design needs from #1048; it is not specified here.

### 4.8 Policy and surface

`BuildLocation` is `control_plane | node` today. L1 and L2 extend it with further variants
and an `executor_ref` that names a pool or a dispatch target, resolved on the existing
environment-over-project chain, so preview environments inherit and production can
override. The settings card follows the repo's discoverability rule: always rendered,
and when no executor is configured it says "No build runner is registered", shows what
a runner would do, and links to the page that enrolls one. It is not added in this
increment, because adding an enum variant changes the OpenAPI schema and both generated
clients and is only honest once something can honour it.

### 4.9 Load and bounds

All of this is control plane, none of it is the proxy hot path. What matters on a 4 GB box:

- **Concurrent remote builds are bounded** per executor, with a per-build queue-wait and
  timeout (a node-level cap is item 5 of #1527).
- **Context is served, not cloned by the runner**: the control plane keeps `DownloadRepoJob`
  and serves the prepared tree, so Dockerfile generation (which inspects the tree) stays
  where the preset engine lives and the runner needs no git access. This is a streamed
  read of an existing archive, bounded by `MAX_BUILD_CONTEXT_BYTES`.
- **Polling is long-poll** with a hold, in the style of route sync, never a tight loop.
- A tar sink streams to the importing daemon and is never held in memory.

### 4.10 Threat model

| Threat | Mitigation |
|---|---|
| Runner reports an image of its choosing | No reference field; reference built server-side from a pre-chosen repository; digest must equal the one the control plane observed |
| Runner replays an old result, or answers for another build | Build id and run handle must equal the dispatch record; the token is single-build and revoked on first result |
| Compromised CI repository | A token is scoped to one build; the builder repository holds no secret and a shim that only receives an opaque id |
| Secrets leak into CI logs | Same rule as worker builds: secret values are never sent; CI log redaction is the provider's, so this is stricter than local |
| Runner floods the log | `MAX_BUILD_EVENT_BYTES`, bounded echo of runner-supplied values |
| Verification failure silently becomes a local build | Typed: `Integrity` and `IdentityRejected` have no fallback path |

## 5. What is deliberately not designed here

- Control-plane replication, ingress HA, worker-side probes and database role discovery:
  #1048.
- Builds producing **Kubernetes** Jobs as a first-class target: listed in #1528 as an
  adapter; it needs a credential model of its own and is not covered.
- A Temps-hosted OCI registry. It would remove the "bring a credential" cost of the
  registry sink, and it is its own ADR. Recommendation: not before the tar sink has
  proven insufficient for a real operator.

## 6. Open questions, with recommendations

| Question (from #718/#1528) | Recommendation |
|---|---|
| Layer extraction on the runner: hand-rolled OCI reading or `skopeo` + `umoci` | Avoid it for phase 1. With the image-config result (#1545) and extraction on the node holding the image, a runner needs no extraction. Revisit only for a registry sink with no daemon. |
| Multi-arch: manifest lists or suffixed tags | Keep suffixed tags (#489) on the tar sink and L0. A registry sink may emit manifest lists, behind the same `image_reference` type. Do not converge the two before a registry sink exists. |
| Temps' own registry | Defer (section 5). |
| Scan optional on the remote path | Keep it non-blocking and optional. The result report must tolerate its absence. |

## 7. Phasing

| Phase | Content | Depends on |
|---|---|---|
| 0 (this PR) | This document; `build_verification` contract and tests | nothing |
| 1 | `temps build-runner`; pull pool with enrollment, leases and heartbeats; tar sink; build-scoped tokens; `BuildLocation` variants and settings card; capabilities endpoint; fallback wiring | #1545 merged (image-config result) |
| 2 | OIDC issuers as rows; GitHub Actions and GitLab CI adapters via a single builder repository; cancel propagation; restart reconciliation | phase 1; #1048's job claim if multiple consoles ship first |
| 3 | Registry sink with encrypted credentials; authenticated pull on the agent; Gitea Actions gated on a version | phase 2 |

Each phase ships behind the `BuildLocation` opt-in, so none changes the default.

## 8. What this increment implements

`crates/temps-deployer/src/build_verification.rs` (new, pure, no I/O):

- `ImageDigest`: strict `sha256:` plus lowercase hex.
- `DispatchedBuild`, `RunnerReport`, `VerifiedBuild` and `verify_runner_report`.
- `RemoteBuildFailure` and `FailureDisposition`, the single statement of which failures
  may fall back.

It is exported from `temps-deployer` and is not called by any deployment path, so it
changes no behaviour. It is the part of this design where a mistake is a security bug
rather than a rework, which is why it is written and tested first, in a module no other
open change touches (#1545 edits `lib.rs`, `remote.rs`, `build_image.rs` and the scan
jobs; #1048 adds one ADR file).

Not implemented: the runner, pools and leases, tokens, issuers and JWT verification,
dispatch adapters, `BuildLocation` variants, settings card, any migration or API change.
