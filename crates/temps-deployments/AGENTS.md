# temps-deployments

Rules for the pipeline that plans a deployment's jobs, runs them and decides when
the revision takes traffic. They add to [`AGENTS.md`](../../AGENTS.md).

## Owns

Planning and running deployments, every job in `src/jobs/`, `DeploymentService`
(rollback, promotion, containers, cancellation), crons, failure reports, and
`PlaneContainerDeployer`, which starts replicas through the workload plane.

It does not own nodes and scheduling (`temps-nodes`); `ContainerDeployer`,
`ImageBuilder` and the Compose executor (`temps-deployer`); job log files
(`temps-deployment-logs`); probe execution (`temps-runtime-health`); workload
reconciliation (`temps-workload-plane`); Traefik discovery
(`temps-traefik-discovery`); deployment tokens and CI dispatch
(`temps-ci-tokens`); Docker cleanup (`temps-docker-housekeeping`). The nodes,
tokens, Traefik and housekeeping crates have no `TempsPlugin`: `DeploymentsPlugin`
mounts their routes and `services/mod.rs` re-exports them, so
`crate::services::NodeScheduler` is code in `temps-nodes`.

## Entry points

- `plugin.rs`: `register_services`, `initialize_plugin_services`, `configure_routes`.
- `services/job_processor.rs`: `JobProcessorService::run`, `gate_check_then_run`.
- `services/workflow_planner.rs`: `create_deployment_jobs` and the `plan_*`
  methods, where a job joins a pipeline and gets its dependencies.
- `services/workflow_execution_service.rs`: `execute_deployment_workflow`; its
  `job_registry/mod.rs` has `JOB_REGISTRY`. `workflow.rs`: `WorkflowTask`.
- `jobs/deploy_image/replica.rs` (`deploy_single_replica`), `jobs/build_image.rs`,
  `jobs/deploy_compose.rs`, `jobs/mark_deployment_complete.rs`.
- `services/services/mod.rs` maps `DeploymentService` by concern.

## Invariants

- **A created container is tracked first.** Nothing fallible runs between the
  create and `track_container`, or cleanup cannot find it:
  `a_log_write_failure_right_after_create_cannot_leak_the_container`,
  `tests/container_cleanup_on_failure_test.rs`.
- **A retained failure never looks ready.** It is stopped, with no `ready_at`:
  `failed_app_container_is_registered_without_becoming_ready`.
- **Only `promote_if_latest` selects a deployment**, through
  `MarkDeploymentCompleteJob`, also from `rollback.rs`, `promote.rs`, `reuse.rs`
  (which can restore the previous one). Otherwise stale work replaces a newer
  route: `test_promote_if_latest_rejects_cancelled_and_superseded_generations`.
- **Only a failed required job stops a workflow.** `WorkflowExecutor` never reads
  a dependency's status, so post-deploy jobs use the completion job as a barrier
  (`plan_git_deployment`). Every early exit closes `Pending` rows, or the UI polls
  forever: `test_aborting_batch_cancels_jobs_that_never_ran`.
- **Job IDs are a contract.** `mark_complete_inner` reads `deploy_container`
  outputs, also written by `DeployComposeJob`; a rename loses container rows.
- **Host Docker access needs an admin** ([ADR 079](../../docs/adr/079-host-docker-socket-grant.md),
  "ADR 045" in code). `create_deployment_jobs` and `DeployImageJobBuilder::build`
  enforce it: `a_project_writer_cannot_build_a_deploy_job_for_a_declared_project`.
- **`job_config` holds no plaintext secrets** (`test_static_build_args_are_sealed`).
  A new sealed field joins `SENSITIVE_JOB_CONFIG_FIELDS`, or failure reports miss it.
- **The processor starts last** in `initialize_plugin_services`, or a queued
  deployment plans with no gate, secrets resolver or audit sink (comment only).
- **A plane replica retires through the plane**, or its workload outlives the
  container: `a_plane_owned_replica_is_retired_through_the_plane`.

## Do not

- Add a job type without a `JOB_REGISTRY` line and table rows
  (`the_job_type_table_covers_exactly_what_the_registry_claims`).
- Infer cancellation from text (`go_context_canceled_is_a_failure_not_a_cancellation`).
- Log resolved variables: `log_redaction.rs` serves only failure reports.

## Testing

See [`README.md`](./README.md). No feature flag. CI runs it all in the
`docker-deployments` group, with a shared `TEMPS_TEST_DATABASE_URL`.

- `TestDatabase::with_migrations` needs Docker or that URL. Guarded tests skip; an
  unguarded `with_migrations().await?` fails.
- Docker, skipping after `docker.ping()`: `container_cleanup_on_failure_test`,
  `nodejs_integration_test`, `workflow_execution_service_test`,
  `multiarch_build_test`. `public_repo_deployment_test` also needs
  `TEMPS_INTEGRATION_TESTS=1`, which CI does not set, so it returns early there.
- No Docker: `multinode_integration_test`, `configure_crons_test`.
