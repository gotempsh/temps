# temps-nodes

The control plane's side of worker nodes. These rules add to
[`AGENTS.md`](../../AGENTS.md), which still applies.

## Owns

- The `nodes` row and its lifecycle (registration, heartbeats, drain state,
  removal), placement, and adding a node (pairing, SSH, mesh enrollment).
- The console's view of the mesh and compute pool, and the control plane's half
  of the node state stream and edge route table.

It does not own:

- Mounting: no `TempsPlugin` here. `temps-deployments/src/plugin.rs` mounts the
  admin routes, `temps-cli` (`serve/console/startup/nodes.rs`) the token routes.
- Draining and failover: `temps-deployments` (`handlers/node_operations.rs`,
  `jobs/node_health_check.rs`).
- CA minting, node clients, enrollment tokens: `temps-config`.
- Mesh state, pairing rows: `temps-network`. Kernel WireGuard: `temps-wireguard`.
- The worker: `temps-node-agent`, `temps-workload-node`, `temps-node-dns`.

## Entry points

- `services/node_service.rs`: `NodeService`, `NodeError`.
- `services/node_scheduler.rs`: `NodeScheduler::schedule_placement`.
- `services/compose_placement.rs`, `services/node_state.rs`,
  `services/wireguard_mesh.rs`, `services/node_pairing_admin.rs`,
  `services/node_ssh_enrollment.rs`; `cluster_ca.rs`: `build_node_deployer`.
- `handlers/nodes.rs`: `configure_routes` (node token), `configure_admin_routes`
  (session, `NodeAdminState`), `NodesApiDoc`.

## Invariants

- Non-empty `target_nodes` or a label selector never fall back to the control
  plane, or a workload lands where it was excluded:
  `test_schedule_with_target_nodes_none_active_fails_closed`,
  `test_schedule_with_labels_no_match_fails_closed`.
- Post-outage and undrain status is decided in the `UPDATE`
  (`STATUS_AFTER_RECOVERY_SQL`, `STATUS_AFTER_UNDRAIN_SQL`), or a racing heartbeat
  restores a cancelled drain: `recovery_status_comes_from_the_row_at_write_time`.
- `remove_with` locks placements, the DNS generation, then the node row before
  counting, or it deadlocks with a DNS publish or misses a new row:
  `test_remove_does_not_deadlock_with_a_concurrent_dns_publish`.
- A registered name or address becomes a SAN on a leaf that is never revoked, so
  a collision impersonates a node or the control plane:
  `identity_conflicts_ignore_name_case`,
  `registration_reserves_the_mesh_pool_while_the_mesh_is_off`.
- A leaked pairing code works only from its reserved mesh address:
  `a_pairing_token_is_redeemed_only_from_the_pairings_mesh_address`. A node token
  acts only for its own node: `one_nodes_token_does_not_act_for_another_node`.
- A deployment planned before a later Compose move stops, or a stack runs where
  nothing stops it: `a_deployment_planned_before_a_later_move_is_refused`.
- An absent heartbeat field keeps the stored value, so an older agent wipes
  nothing: `test_heartbeat_preserves_dns_resolver_fields_when_absent`.

## Do not

- Depend on `temps-deployments`, which depends on this crate. `POST .../drain`
  lives there on a path shared with routes here
  (`the_merged_drain_path_serves_status_drain_and_undrain`).
- Put operator input or a secret in an SSH command's text
  (`the_sudo_password_never_reaches_the_wrapped_command`).
- Say why an SSH connection failed, which makes the endpoint a network scanner
  (`connect_failures_all_read_the_same_and_name_no_address_or_cause`).
- Remove a container without `remove_owned_container`'s label check.
- Add inventory to `node_capability`, session-only on purpose (see its comment).
- Add a route nodes call over the mesh without listing it in `is_node_route`
  (`temps-cli/src/commands/serve/node_api.rs`); unlisted paths answer 404.
- Drop `project_permission_guard!` from `handlers/compose_placement.rs`:
  `project_permission_guard_coverage_snapshot` (`temps-authz`) pins the crate.

## Testing

`cargo test --lib -p temps-nodes <name>`; lib tests only, no feature needed.
`test-support` lends `temps-deployments` the `admin_test_support` router harness.
`TestDatabase` tests need `TEMPS_TEST_DATABASE_URL` or Docker. `migrated_test_db`
(`services/node_service.rs`, `handlers/nodes.rs`) skips when both are missing and
panics on a failed setup; the others skip on any setup error. CI runs the crate
in **docker-deployments** only, through `package(temps-nodes)`.
