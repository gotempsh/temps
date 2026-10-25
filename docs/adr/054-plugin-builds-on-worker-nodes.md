# ADR 054: Plugin builds run on a worker node when the control plane has no Docker

## Status

Proposed — 2026-10-19

Related: ADR 052 (worker health), the `control-plane` serve profile
(`LocalWorkloadPolicy::control_plane`), external plugins
(`crates/temps-external-plugins`).

## Context

Installing an external plugin from a GitHub repository fetches the exact
commit, then compiles it with Bun inside a pinned, resource-bounded Docker
container: no host mounts, read-only root, `--cap-drop ALL`,
`no-new-privileges`, 1 CPU / 1.5 GiB, and the network cut before the
repository's own code is compiled (`repository::build_with_progress`).

A control plane started with `--profile control-plane` (a hosted plane) runs no
workloads and has no Docker. The install failed there with a generic
"build failed", and before that, because the image shipped no `git`, with
"GitHub could not resolve or download the requested repository commit".
Neither message named the cause.

## Decision

1. **Say what is missing.** A missing `docker` or `git` executable is a
   property of the server, not of the repository. They are reported as
   `503 Plugin Builder Unavailable` and `503 Git Unavailable` with text that
   names the fix. The official image ships `git`.
2. **Build on a worker.** When the local `docker` is absent, the control plane
   asks a joined worker to compile. The control plane archives the fetched
   source (tar, capped at 128 MiB), sends it to
   `POST /agent/plugins/build?target=<bun target>` on the worker's agent over
   the existing bearer-token + mutual-TLS channel, and receives the binary
   (capped at 256 MiB). The worker runs the *same* `build_in_container`
   routine; there is one build recipe, not two.
3. **The control plane still hashes and verifies.** The binary's SHA-256 is
   computed by the control plane after receipt and goes into the receipt, and
   the candidate is started and verified exactly as for a local build.
4. **The archive is never unpacked on the worker's filesystem.** The agent
   writes it to a private temporary directory and streams it into the build
   container, which extracts it into its own tmpfs.
5. **Node choice.** Build-only nodes (`temps.sh/role=builder`) first, then
   ordinary workers, each by id. Dedicated nodes and the control plane itself
   are skipped. A worker that cannot build (no Docker, busy, an agent that
   predates the endpoint, unreachable) is skipped and the next is tried; a
   worker that took the build and failed it ends the attempt with its reason.
   The compile target is the *control plane's* platform, so the worker's
   architecture does not matter.
6. **One build at a time per worker**, and only the four Linux Bun targets.

The seam is `temps_core::plugin_builder::RemotePluginBuilder`, registered by the
deployments plugin and looked up by the external-plugins plugin, so
`temps-external-plugins` does not depend on node scheduling.

## Security

- The worker already runs the customer's code; compiling a plugin there is no
  new trust. A worker that returns a malicious binary can compromise what a
  worker that returns a malicious deployment image already can: the plugin
  runs inside the control plane process tree, so this *is* a new trust edge
  from worker to control plane. It is bounded by the install flow's existing
  gates (sensitive-action step-up, the plugin's declared grants, candidate
  start-up verification). It is **not** bounded by who picks the worker: the
  builder is chosen from labels the worker reports itself, so a compromised
  worker can make sure it is the one asked, and its binary then runs with the
  control plane's privileges.
- **Accepted for now (2026-10-20).** A worker build only happens when the
  control plane has neither Docker nor Podman; self-hosted servers have
  Docker and hosted control planes build with Podman, so neither takes this
  path. If it becomes common, a follow-up restricts building to nodes an
  administrator marks as builders and shows the builder node and the
  binary's SHA-256 in the install step-up.
- The bearer token and mTLS client identity are the existing ones; the route
  requires the deployment-create permission like image builds.
- Source is size-capped on both sides; the binary is size-capped and
  length-checked.

## Consequences

- A hosted control plane can install plugins as soon as one worker with Docker
  has joined, which is also the precondition for deploying anything.
- A control plane with no workers answers with an actionable message instead of
  a failure.
- Not done here: a remote *service* builder for control planes that will never
  have a worker; a signed build attestation from the worker.
