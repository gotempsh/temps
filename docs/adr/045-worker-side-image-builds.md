<!-- SCOPE: Defines how a control plane that owns no Docker daemon gets an application image built, and why the build executes on a worker node rather than on the control plane. -->

# ADR-045: Worker-side image builds for the control-plane profile

**Status:** Proposed
**Date:** 2026-09-17
**Author:** Temps Contributors

> Security-sensitive: this ADR proposes moving application source code and
> build-time secrets across the control-plane → worker trust boundary for the
> first time. Implementation requires `security-auditor` sign-off on the
> credential-handling design in §4 before it starts.

## Context

`temps serve --profile control-plane` (PR #1031) lets the control plane run in
a container with no Docker socket and no `DOCKER_HOST`. The process constructs
no bollard client at all: every daemon-dependent path resolves a
`temps_core::DockerHandle` and fails with a typed `DockerUnavailable` rather
than panicking or silently attempting a local container.

That profile exists so a hosted control plane can be one container per tenant,
managing remote worker nodes over the agent. It is only useful if an
application can actually reach a worker node. Two delivery paths matter:

1. **A prebuilt image in a registry.** PR #1031 adds `POST /agent/images/pull`
   so the worker pulls the image itself. No control-plane daemon is involved.
   This path works today.

2. **A build from source (git repository, Dockerfile or preset).** This does
   not work, and this ADR is about that gap.

### Why building from source does not work

The build path is control-plane-only by construction:

- `crates/temps-deployments/src/jobs/build_image.rs` runs the build against the
  control plane's own daemon through the `ImageBuilder` trait.
- `crates/temps-deployments/src/jobs/deploy_image.rs` (`ensure_image_on_remote`)
  then runs `docker save` on the control plane and streams the tarball to the
  worker's `POST /agent/images/import`.
- `crates/temps-deployer/src/remote.rs` `RemoteNodeDeployer::build_image()`
  explicitly returns *"Build image not supported on remote nodes — images are
  built on the control plane"*. The agent exposes no build endpoint at all.

So both the build and the transfer require a daemon on the control plane. In
the control-plane profile there is none, and there is no correct answer to give
a user who pushes to a git-backed project.

### What PR #1031 does about it in the interim

Nothing silent, and nothing that reaches the daemon path first: `BuildImageJob`
refuses under the profile with a typed `LocalWorkloadsDisabled` error whose
message names the remedy (deploy from a registry image, or run the full profile
on a node with Docker). The refusal surfaces in the deployment log and the
deployment's failure state, not only in the job result. That is an enforced
contract, not a documented limitation — but it is still a capability gap, and
this ADR is how it gets closed rather than forgotten.

## Decision

Execute the build **on the worker node that will run the resulting image**, via
the existing authenticated agent channel. The image then never leaves the node
that needs it, which also deletes the `docker save` + stream transfer for the
single-target case.

### 1. Agent build endpoint

A new authenticated route on the agent, alongside
`POST /agent/images/import` and `POST /agent/images/pull`:

```
POST /agent/images/build
```

It accepts the build inputs, runs the agent's existing local `ImageBuilder`
against the node's own daemon, and streams build output back. The agent already
links `temps-deployer` and instantiates an `ImageBuilder`, so the build logic
itself is reused rather than reimplemented.

Concurrency, size limits and timeouts follow the `import_image` precedent: a
semaphore-bounded slot count, an explicit byte ceiling on the source archive,
and a build timeout — a build is more expensive than an import, not less, and
an unbounded one is a denial-of-service against a tenant's worker.

### 2. Source transfer

The control plane already materialises the repository on disk in the
`download_repo` job. It archives that tree and streams it to the agent, which
unpacks it into a scratch directory, builds, and deletes it. Reusing the
existing backpressure-aware streaming body from the import path keeps memory
constant on both ends.

The alternative — having the agent clone from git itself — is rejected:
it would put git credentials on every worker node and duplicate the control
plane's provider/auth handling, widening the blast radius for no benefit.

### 3. Build log streaming

Builds are long and users watch them. The agent streams build output back
line-by-line and the control plane forwards it into the deployment log through
the existing `LogCallback` used by local builds, so the console experience is
unchanged whether the build ran locally or on a worker.

### 4. Build arguments and secrets (security-sensitive)

This is the part that needs `security-auditor` sign-off before implementation.

Today, build args and BuildKit secrets are decrypted on the control plane and
handed to a local daemon. Under this ADR they cross the network to a worker.
The design must state, and the review must confirm:

- secrets travel only over the authenticated, encrypted agent channel;
- the agent never writes them to disk unencrypted, never logs them, and never
  echoes them in an error or in build output;
- they are scoped to the single build and destroyed with the scratch directory;
- a compromised or malicious worker node's blast radius is bounded and
  documented — a node that can request builds must not thereby obtain secrets
  for projects it does not host.

Until that review is complete this ADR stays **Proposed**.

### 5. Multi-architecture and multi-node deployments

`required_build_platforms()` derives the platforms a deployment must cover from
the architectures the target nodes report through the agent heartbeat
(`nodes.architecture`), not from a local daemon — PR #1031 already makes that
change.

A deployment that targets nodes of **one** architecture builds once, on a
target node, and stays there. A deployment that spans **several** nodes needs
the image on each. Options, in preference order:

1. Build once per distinct architecture on one node of each, and have the other
   nodes pull from a registry when one is configured.
2. Agent-to-agent transfer, if and when that exists.
3. Otherwise: refuse with a typed, actionable error naming the limitation.

What is explicitly rejected is routing the image through the control plane's
daemon, which is the thing this profile does not have.

## Consequences

**Positive.** The control-plane profile becomes able to deploy from source. The
`docker save` + stream transfer disappears for the common single-target case,
removing a full image-sized round trip through the control plane. Build load
moves off the control plane onto the nodes that were sized for workloads.

**Negative.** Source code and build secrets reach worker nodes, which is a real
widening of the trust boundary and the reason for §4. Build failures now have a
network hop in the middle, so the error surface grows: the console must
distinguish "the build failed" from "the node became unreachable mid-build".
Build caching becomes per-node rather than shared.

**Size.** Roughly 1,200–1,500 lines across `crates/temps-agent/`
(`handlers.rs`, `server.rs`, a new build handler module),
`crates/temps-deployer/` (`lib.rs`, `remote.rs`) and
`crates/temps-deployments/` (`jobs/build_image.rs`, `jobs/deploy_image.rs`),
plus tests. This is why it is a separate change from PR #1031 rather than part
of it.

## Verification

Implementation is not complete until all of the following are demonstrated with
real output, not asserted:

- A control plane running in a container with **no Docker socket and no
  `DOCKER_HOST`** builds and deploys a git-backed application onto a joined
  worker node, with the build log visible in the console.
- The built image exists on the worker and **not** on the control plane.
- A build failure on the worker surfaces as a failed deployment with the real
  compiler/Dockerfile error in the deployment log.
- A worker that disconnects mid-build produces a distinct, actionable failure
  rather than a hung deployment.
- Build secrets appear in neither the agent's logs nor the deployment log.
- The `full` profile's behaviour is byte-for-byte unchanged.

## Related

- PR #1031 — the control-plane serve profile, the `DockerHandle` seam, the
  `POST /agent/images/pull` registry path, and the typed build refusal this ADR
  supersedes.
- `docs/features/multi-node/page.mdx` — user-facing description of the profile
  and its current build limitation.
- Issue #1034 — tracking issue for this ADR.
