# Issue #1097: Remote promotion and rollback

Issue: https://github.com/gotempsh/temps/issues/1097

## Problem

Promotion and rollback used the control plane's Docker cache as an admission
check and built inline deployment jobs without worker scheduling or remote pull
configuration. Daemon-less control planes therefore returned 500 even when a
compatible worker could run the artifact. Static deployments were rejected
before their image-less path, and failures after insertion could leave a new
deployment running indefinitely.

## Implementation

- Reused images are verified or pulled by the selected execution node; the
  control plane no longer probes its local image cache.
- Reuse jobs receive the normal node scheduler, placement constraints,
  anti-affinity, encryption/config services, image-transfer support, remote
  environment rewrites, and registry credentials only for an exact registry
  host match.
- Static promotion and rollback are selected before image validation, preserve
  their asset lineage, and atomically activate the completed deployment.
- Every post-admission error persists a failed terminal state with a finish
  timestamp and contextual reason while preserving an existing cancellation or
  supersession outcome.
- The active deployment remains selected and running until its replacement
  passes completion. Failed candidates cannot tear down healthy production.
- Local-only Git build tags rebuild the recorded commit because workers cannot
  recover a pruned tag from a registry.
- Static reuse passes through the normal generation fence and route reload
  confirmation before it is marked completed.
- Completion errors are reconciled against the selected route: an already
  completed live release remains successful, while an incomplete candidate
  restores and confirms the prior usable route without overwriting newer work.
- `DeploymentService` shares the scheduler, workflow planner, and image builder
  used by ordinary workflow execution.

## Verification

- `cargo test --lib -p temps-deployments`: 943 passed, 0 failed, 3 ignored.
- `cargo test --lib -p temps-deployments rollback -- --nocapture`: 11 passed.
- Focused regressions passed for pruned local Git rebuild, cancellation
  preservation, static generation fencing, and confirmed static route reload.
- `cargo test --lib -p temps-deployments failed_rollback_keeps_current_deployment_active -- --nocapture`: 1 passed.
- `cargo test --lib -p temps-deployments test_rollback_does_not_probe_control_plane_image_cache -- --nocapture`: 1 passed.
- `cargo test --lib -p temps-deployments static_promotion_reuses_assets_without_an_image -- --nocapture`: 1 passed.
- `cargo test --lib -p temps-deployments reuse_setup_failure_persists_terminal_deployment_state -- --nocapture`: 1 passed.
- `cargo check --lib -p temps-deployments`: passed without crate warnings.
- Isolated `start-temps` slot 6 used a fresh database. Migrations and plugin
  initialization completed, `GET http://127.0.0.1:8140/health` returned HTTP
  200, and the server shut down cleanly.

The isolated slot had no enrolled worker, so it could not perform a live
cross-host promotion. Worker selection, remote pulling, registry isolation,
cross-node environment rewriting, and failure handling are covered by the
deployments crate regression suite.

Security review remains required before merge because the change handles node
scheduling, registry credentials, and worker-owned container cleanup.
