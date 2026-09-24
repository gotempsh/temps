# Issue #1096: Reconcile worker container ports after Docker restart

Issue: https://github.com/gotempsh/temps/issues/1096

## Implementation

- Worktree: `/Users/davidviejo/projects/temps/temps-wt-1096-refresh-container-ports`
- Branch: `fix/1096-refresh-container-ports`
- Deliverable: a verified fix and one focused PR targeting `main`, referencing `Fixes #1096`. Do not merge automatically.

Read workspace and repository `AGENTS.md`, `CLAUDE.md`, and applicable `bugfix` / `pr-evidence` guidance. You are not alone in the workspace: preserve existing edits, including the explicitly listed work below, and coordinate overlapping files with the other issue agents. Use only this issue's worktree; the original `temps/` checkout contains unrelated dirty files and local secrets.

## Problem and reproduction

Docker assigns published host ports dynamically. After a worker Docker restart, the same running container can have a new host port while `deployment_containers.host_port` and the proxy route retain the old value. The node reports healthy but requests remain 503 until redeployment.

Use disposable worker resources: deploy an image, record its published port and route, induce a changed published port, and verify routing recovers without redeployment within a monitoring interval plus route reload latency. Never restart the host's existing Docker daemon for this test.

## Confirmed root cause and important correction

`crates/temps-monitoring/src/container_health.rs::persist_runtime_info` stores only start time and CPU limit. However, changing that method alone does NOT fix the reported remote-worker topology: the monitor is constructed with the local deployer in `crates/temps-cli/src/commands/serve/console.rs`.

The heartbeat is not an existing recovery mechanism either: `crates/temps-agent/src/server.rs` sends inventory on initial startup only, strips each container to ID/name, and `NodeService::reconcile_containers` only removes ghost records. A Docker restart while the agent remains running is not reported with ports.

Existing reusable remote access:

- `crates/temps-deployments/src/services/services.rs::deployer_for_node` and `container_operations_for_node` resolve persisted `node_id`, decrypt node credentials, and build the mTLS-aware remote deployer.
- `RemoteNodeDeployer` already implements container info AND stats.
- `crates/temps-routes/src/route_table.rs` consumes host ports and listens on PostgreSQL `route_table_changes`. There is no existing deployment-container update trigger sufficient for this change.

## Implemented architecture

1. Define a small async runtime-resolver trait and typed contextual error in `temps-monitoring`. Do not introduce a reverse dependency on `temps-deployments`, which already depends on monitoring.
2. Implement that trait for `DeploymentService`, delegating to its existing node-aware container-operation resolver.
3. Register the trait object in the deployments plugin and attach it when console startup constructs `ContainerHealthMonitor`.
4. In each monitoring cycle, resolve/cache deployers by distinct node ID. Use the SAME selected deployer for both `get_container_info` and `get_container_stats`; changing only info leaves remote metrics broken.
5. Local rows use the local deployer. A remote-resolution failure must skip/log that worker with node/container context, never fall back to local Docker. One unreachable worker must not abort other workers' checks.
6. Match the stored container port to a nonzero TCP published mapping. Preserve the old host port if no suitable mapping exists. Handle ambiguous multiple-interface mappings deterministically and consistently with the route's reachable node address.
7. Persist changed ports and emit route notification in one transaction, so a notification failure rolls back the port and the next poll retries. Avoid writes/notifications when nothing changed; batch notifications where practical.

This approach needs no heartbeat or OpenAPI schema change. Scope resolver caching to the polling cycle so token/CA rotation is respected. Keep background work bounded and proportional to nodes/containers being checked.

The monitoring crate now exposes a small node runtime resolver trait. `DeploymentService`
implements it through the existing node-aware, mTLS-capable container operation path, and
the deployments plugin registers the trait for console startup. Each poll resolves a worker
once, caches the result only for that cycle, and uses the same selected deployer for container
info and stats. Resolution failures skip only the affected worker containers and never fall
back to control-plane Docker.

Published ports are selected from matching, nonzero TCP bindings with deterministic interface
ordering. Changes are persisted with runtime metadata and `route_table_changes` notification
inside one transaction; missing mappings preserve the prior port and unchanged ports avoid
writes and notifications.

## Acceptance and evidence

- A worker port change heals without redeploy; both runtime info and stats are requested from that worker, never CP Docker.
- Local deployments retain correct behavior; no valid mapping preserves the previous port.
- Matching TCP, UDP-only, other-container-port, zero-port, unchanged, and multiple-interface cases are covered.
- Update or notify errors do not silently leave persistence and routes inconsistent; retry is demonstrated.
- Node resolution is bounded per cycle and unrelated workers continue on one failure.
- Run affected library tests/checks for monitoring, deployments, and CLI wiring. Resolver and
  deployer selection are covered with inline mocks; the existing remote-deployer HTTP path is
  reused unchanged. A disposable second worker was unavailable for an end-to-end Docker port
  reassignment reproduction.
- Review security of node credential handling, CA rotation, and node ownership. Coordinate with #1097 because both may edit DeploymentService/plugin wiring, but keep commits isolated.

## Verification

Exact commands and results are recorded in the pull request Evidence section. Verification
includes focused reconciliation tests, the affected crate library suites, required `cargo check
--lib`, and an isolated `/start-temps` readiness smoke test. The unavailable live two-node
Docker restart reproduction is disclosed rather than inferred.
