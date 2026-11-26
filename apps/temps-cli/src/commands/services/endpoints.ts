// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps services` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  addClusterMember,
  applyServiceHealthcheck,
  cancelPgUpgrade,
  downloadObject,
  externalServiceResetPgStatStatements,
  getClusterHealth,
  getClusterMember,
  getHealthcheckDefaults,
  getManagedCaCertificate,
  getPgUpgrade,
  getPgUpgradeLogs,
  getProjectServiceEnvironmentVariables,
  getProviderMetadata,
  getProvidersMetadata,
  getQueryContainerInfo,
  getServiceHealthStatus,
  getServiceHealthcheck,
  getServiceLogIngestion,
  getServicePreviewEnvironmentVariableNames,
  getServicePreviewEnvironmentVariablesMasked,
  getServiceRuntime,
  getServiceStats,
  getServiceTls,
  getWorkloadMode,
  getWorkloadPendingChanges,
  issueRuntimeCredentials,
  listAvailableContainers,
  listPgUpgrades,
  listProjectServices,
  listServiceHealthStatuses,
  migrateS3Service,
  planRestore,
  promoteClusterMember,
  queryData,
  recreateFromWorkloadSpec,
  removeClusterMember,
  retryCluster,
  retryPgUpgrade,
  revealServiceEnvironmentVariables,
  revealServiceParameter,
  rollbackPgUpgrade,
  startPgUpgrade,
  testWalArchiveTarget,
  triggerServiceHealthCheck,
  updateHealthcheckDefaults,
  updateServiceHealthcheck,
  updateServiceLogIngestion,
  updateServiceResources,
  updateServiceTls,
  updateWorkloadMode,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerServicesEndpoints(program: Command): void {
  const group = topGroup(program, 'services', "services")

  endpoint(group, listAvailableContainers, {
    name: "list-available-containers",
    description: "List available Docker containers that can be imported as services",
  })

  endpoint(group, listServiceHealthStatuses, {
    name: "list-health-statuses",
    description: "Current health status for many services at once",
    query: {
      ids: { kind: 'string', help: "Comma-separated service IDs" },
    },
  })

  endpoint(group, getHealthcheckDefaults, {
    name: "get-healthcheck-defaults",
    description: "Get the instance-wide default healthcheck of managed service containers",
  })

  endpoint(group, updateHealthcheckDefaults, {
    name: "update-healthcheck-defaults",
    description: "Replace the instance-wide default healthcheck of managed service containers",
    body: {
      interval_secs: { kind: 'int', help: "Seconds between checks once healthy (5–3600)" },
      retries: { kind: 'int', help: "Consecutive failures that make the container unhealthy (1–10)" },
      start_interval_secs: { kind: 'int', help: "Seconds between checks while starting (1–60)" },
      start_period_secs: { kind: 'int', help: "Seconds after start during which failures do not count (0–600)" },
      timeout_secs: { kind: 'int', help: "Seconds one check may run (1–60, and shorter than the interval)" },
    },
  })

  endpoint(group, listProjectServices, {
    name: "list-project-services",
    description: "List services linked to a project",
    path: { project_id: 'project' },
    query: {
      page: { kind: 'int', help: "Page number (1-indexed)" },
      page_size: { kind: 'int', help: "Number of items per page (max 100)" },
      sort_by: 'string',
      sort_order: 'string',
    },
  })

  endpoint(group, getProjectServiceEnvironmentVariables, {
    name: "get-environment-variables",
    description: "Get all environment variables for all services linked to a project",
    path: { project_id: 'project' },
  })

  endpoint(group, getProvidersMetadata, {
    name: "get-providers-metadata",
    description: "Get provider metadata (display names, icons, descriptions)",
  })

  endpoint(group, getProviderMetadata, {
    name: "get-provider-metadata",
    description: "Get metadata for a specific provider",
    path: { service_type: 'string' },
  })

  endpoint(group, getClusterHealth, {
    name: "get-cluster-health",
    description: "Per-member health for a Postgres HA cluster",
    path: { id: 'int' },
  })

  endpoint(group, revealServiceEnvironmentVariables, {
    name: "reveal-environment-variables",
    description: "Reveal a service's basic environment variables in plaintext, before it is linked to any project",
    path: { id: 'int' },
  })

  endpoint(group, triggerServiceHealthCheck, {
    name: "trigger-health-check",
    description: "Run a health check for one service right now",
    path: { id: 'int' },
  })

  endpoint(group, getServiceHealthStatus, {
    name: "get-health-status",
    description: "Persisted health status for an external service",
    path: { id: 'int' },
    query: {
      limit: { kind: 'int', help: "Max number of recent checks (default 50, max 200)" },
    },
  })

  endpoint(group, getServiceHealthcheck, {
    name: "get-healthcheck",
    description: "Get a managed service's healthcheck",
    path: { id: 'int' },
  })

  endpoint(group, updateServiceHealthcheck, {
    name: "update-healthcheck",
    description: "Replace a managed service's healthcheck overrides",
    path: { id: 'int' },
    body: {
      interval_secs: { kind: 'int', help: "Seconds between checks once healthy (5–3600)" },
      retries: { kind: 'int', help: "Consecutive failures that make the container unhealthy (1–10)" },
      start_interval_secs: { kind: 'int', help: "Seconds between checks while starting (1–60)" },
      start_period_secs: { kind: 'int', help: "Seconds after start during which failures do not count (0–600)" },
      timeout_secs: { kind: 'int', help: "Seconds one check may run (1–60, and shorter than the interval)" },
    },
  })

  endpoint(group, applyServiceHealthcheck, {
    name: "apply-healthcheck",
    description: "Recreate a managed service's container to apply its healthcheck now",
    path: { id: 'int' },
  })

  endpoint(group, getServiceLogIngestion, {
    name: "get-log-ingestion",
    description: "Get this service's own container log ingestion budget",
    path: { id: 'int' },
  })

  endpoint(group, updateServiceLogIngestion, {
    name: "update-log-ingestion",
    description: "Replace this service's container log ingestion budget",
    path: { id: 'int' },
    body: {
      log_ingest_kib_per_sec: { kind: 'int', help: "KiB of log text per second per container (0–1,048,576). null = platform default" },
      log_ingest_lines_per_sec: { kind: 'int', help: "Lines per second per container (0–1,000,000). null = platform default" },
    },
  })

  endpoint(group, addClusterMember, {
    name: "add-cluster-member",
    description: "Begin adding a single new member to a running cluster",
    path: { id: 'int' },
    body: {
      node_id: { kind: 'int', help: "Target worker node ID" },
      role: { kind: 'string!', help: "Member role" },
    },
  })

  endpoint(group, removeClusterMember, {
    name: "remove-cluster-member",
    description: "Remove a single member from a running cluster",
    path: { id: 'int', member_id: 'int' },
    confirm: "This will remove a single member from a running cluster.",
  })

  endpoint(group, getClusterMember, {
    name: "get-cluster-member",
    description: "Get a single cluster member's current state",
    path: { id: 'int', member_id: 'int' },
  })

  endpoint(group, promoteClusterMember, {
    name: "promote-cluster-member",
    description: "Promote a replica to primary by triggering a pg_auto_failover failover",
    path: { id: 'int', member_id: 'int' },
  })

  endpoint(group, migrateS3Service, {
    name: "migrate-s3",
    description: "Copy every bucket of another managed S3 service into this one",
    path: { id: 'int' },
    body: {
      source_service_id: { kind: 'int!', help: "Managed S3 service to copy from (RustFS or Garage)" },
    },
  })

  endpoint(group, revealServiceParameter, {
    name: "reveal-parameter",
    description: "Reveal one sensitive service parameter",
    path: { id: 'int', param_name: 'string' },
  })

  endpoint(group, getServicePreviewEnvironmentVariablesMasked, {
    name: "get-preview-environment-variables-masked",
    description: "Get environment variables preview with masked sensitive values",
    path: { id: 'int' },
  })

  endpoint(group, getServicePreviewEnvironmentVariableNames, {
    name: "get-preview-environment-variable-names",
    description: "Get environment variable names preview (safe - no sensitive values)",
    path: { id: 'int' },
  })

  endpoint(group, issueRuntimeCredentials, {
    name: "issue-runtime-credentials",
    description: "Issue live connection credentials for a service in an environment",
    path: { id: 'int', project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, updateServiceResources, {
    name: "update-resources",
    description: "Update a service's resource limits (memory, CPU caps)",
    path: { id: 'int' },
    body: {
      cpu_shares: { kind: 'int', help: "Relative CPU weight (default 1024)" },
      memory_mb: { kind: 'int', help: "Hard memory limit in MiB" },
      memory_swap_mb: { kind: 'int', help: "Memory + swap limit in MiB" },
      nano_cpus: { kind: 'int', help: "CPU quota in nano-cpus. 1_000_000_000 = 1 full CPU core" },
      shm_size_mb: { kind: 'int', help: "Shared memory (/dev/shm) size in MiB" },
    },
  })

  endpoint(group, planRestore, {
    name: "restore plan",
    description: "Plan restore",
    path: { id: 'int' },
    body: {
      mode: { kind: 'string!', values: ["in_place","new_service","pitr"] },
      name: { kind: 'string', help: "Name for the new service" },
      parameter_overrides: { kind: 'json', help: "Optional parameter overrides (port, docker_image, database)" },
      new_service_name: { kind: 'string', help: "Required when 'to_new_service' is true" },
      target: { kind: 'json', help: "Recovery target kind + value" },
      to_new_service: { kind: 'bool', help: "Whether PITR restores in place or creates a new service" },
      backup_engine: { kind: 'string', help: "Engine of the backup when specified by 'backup_location' (\"postgres\", \"redis\", \"mongodb\", \"s3\")" },
      backup_id: { kind: 'int', help: "DB id of the backup to restore from" },
      backup_location: { kind: 'string', help: "Raw S3 URL / key of the backup — used when restoring a backup discovered by S3 scan (i.e., produced by..." },
      confirm_cross_service: { kind: 'bool', help: "Explicit confirmation for a destructive cross-service restore" },
      s3_source_id: { kind: 'int', help: "S3 source the 'backup_location' lives in" },
    },
  })

  endpoint(group, retryCluster, {
    name: "retry-cluster",
    description: "Retry a failed cluster service initialization",
    path: { id: 'int' },
    body: {
      members: { kind: 'json', help: "Cluster member specifications (same format as create)" },
    },
  })

  endpoint(group, getServiceRuntime, {
    name: "get-runtime",
    description: "Inspect a service's container(s): status, restart count, OOM-killed flag, exit code, and the cgroup limits..",
    path: { id: 'int' },
  })

  endpoint(group, getServiceStats, {
    name: "get-stats",
    description: "Sample current CPU/memory usage from each of a service's containers",
    path: { id: 'int' },
  })

  endpoint(group, getServiceTls, {
    name: "get-tls",
    description: "Get service tls",
    path: { id: 'int' },
  })

  endpoint(group, updateServiceTls, {
    name: "update-tls",
    description: "Update service tls",
    path: { id: 'int' },
    body: {
      mode: { kind: 'string!', help: "The new mode. 'required' is rejected until it is supported", values: ["off","available","required"] },
    },
  })

  endpoint(group, getManagedCaCertificate, {
    name: "get-managed-ca-certificate",
    description: "Get managed ca certificate",
    path: { id: 'int' },
  })

  endpoint(group, testWalArchiveTarget, {
    name: "test-wal-archive-target",
    description: "Test wal archive target",
    path: { id: 'int' },
  })

  endpoint(group, getWorkloadMode, {
    name: "get-workload-mode",
    description: "Get a database's workload mode (internal, operator-only)",
    path: { id: 'int' },
  })

  endpoint(group, updateWorkloadMode, {
    name: "update-workload-mode",
    description: "Change a database's workload mode (internal, operator-only)",
    path: { id: 'int' },
    body: {
      mode: { kind: 'string!', values: ["legacy","shadow","enforce"] },
    },
  })

  endpoint(group, getWorkloadPendingChanges, {
    name: "get-workload-pending-changes",
    description: "List what a recreate from the workload spec would change (internal, operator-only)",
    path: { id: 'int' },
  })

  endpoint(group, recreateFromWorkloadSpec, {
    name: "recreate-from-workload-spec",
    description: "Recreate a database's container from its workload spec (internal, operator-only)",
    path: { id: 'int' },
    body: {
      confirmed_fingerprint: { kind: 'string', help: "The 'fingerprint' of the pending changes being confirmed" },
    },
  })

  endpoint(group, externalServiceResetPgStatStatements, {
    name: "external-reset-pg-stat-statements",
    description: "Reset all statistics accumulated by 'pg_stat_statements' for a Postgres service",
    path: { service_id: 'int' },
    body: {
      confirm: { kind: 'bool!', help: "Must be 'true' to acknowledge the global, irreversible reset" },
    },
  })

  endpoint(group, queryData, {
    name: "query data",
    description: "Query data from an entity with optional filters, pagination, and sorting",
    path: { service_id: 'int', path: 'string', entity: 'string' },
    body: {
      filters: { kind: 'json', help: "JSON filters (backend-specific format)" },
      limit: { kind: 'int', help: "Maximum number of rows to return" },
      offset: { kind: 'int', help: "Number of rows to skip" },
      sort_by: { kind: 'string', help: "Sort by field name" },
      sort_order: { kind: 'string', help: "Sort order (asc/desc)" },
    },
  })

  endpoint(group, downloadObject, {
    name: "query download-object",
    description: "Download an object (S3 only) as a streaming response",
    path: { service_id: 'int', path: 'string', entity: 'string' },
    output: 'bytes',
  })

  endpoint(group, getQueryContainerInfo, {
    name: "query get-container-info",
    description: "Get information about a specific container",
    path: { service_id: 'int', path: 'string' },
  })

  endpoint(group, listPgUpgrades, {
    name: "pg-upgrade list",
    description: "List recent upgrades for a single service (newest first, page size 50)",
    path: { service_id: 'int' },
  })

  endpoint(group, startPgUpgrade, {
    name: "pg-upgrade start",
    description: "Start a new PostgreSQL major-version upgrade for a service",
    path: { service_id: 'int' },
    body: {
      from_image: 'string!',
      from_version: 'string!',
      to_image: 'string!',
      to_version: 'string!',
    },
  })

  endpoint(group, getPgUpgrade, {
    name: "pg-upgrade get",
    description: "Get a single upgrade by id, scoped to a service",
    path: { service_id: 'int', id: 'int' },
  })

  endpoint(group, cancelPgUpgrade, {
    name: "pg-upgrade cancel",
    description: "Cancel an in-flight upgrade",
    path: { service_id: 'int', id: 'int' },
    confirm: "This will cancel an in-flight upgrade.",
  })

  endpoint(group, getPgUpgradeLogs, {
    name: "pg-upgrade get-logs",
    description: "Get the accumulated JSONL log content for an upgrade (for dashboard display)",
    path: { service_id: 'int', id: 'int' },
  })

  endpoint(group, retryPgUpgrade, {
    name: "pg-upgrade retry",
    description: "Retry a failed upgrade",
    path: { service_id: 'int', id: 'int' },
  })

  endpoint(group, rollbackPgUpgrade, {
    name: "pg-upgrade rollback",
    description: "Roll a completed upgrade back to its pre-upgrade PGDATA volume and old image",
    path: { service_id: 'int', id: 'int' },
    confirm: "This will roll a completed upgrade back to its pre-upgrade PGDATA volume and old image.",
  })
}
