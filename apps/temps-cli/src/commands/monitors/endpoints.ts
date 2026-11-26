// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps monitors` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  createCronMonitor,
  createHttpCheck,
  deleteCronMonitor,
  deleteHttpCheck,
  detectEnvCredential,
  detectSecretCredential,
  getBucketedStatus,
  getCronMonitor,
  getHttpChecksCapabilities,
  getProjectsMonitorHealth,
  listCronMonitorRuns,
  listCronMonitors,
  listHttpCheckPresets,
  listHttpChecks,
  listSecretHistory,
  listVariableHistory,
  runHttpCheck,
  setHttpCheckEnabled,
  updateCronMonitor,
  updateHttpCheck,
  updateMonitor,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerMonitorsEndpoints(program: Command): void {
  const group = topGroup(program, 'monitors', "monitors")

  endpoint(group, getProjectsMonitorHealth, {
    name: "get-health",
    description: "Get monitor-based health summaries for multiple projects in a single query",
    query: {
      project_ids: { kind: 'string!', help: "Comma-separated list of project IDs" },
      environment_id: { kind: 'int', help: "Select one environment within the authorized projects; defaults to production" },
    },
  })

  endpoint(group, updateMonitor, {
    name: "update",
    description: "Update a monitor's name, check interval or check path",
    path: { monitor_id: 'int' },
    body: {
      check_interval_seconds: { kind: 'int', help: "How often to probe, in seconds. **Clamped, not rejected**, exactly like on create: values below 30 are..." },
      check_path: { kind: 'string', help: "Path probed on the monitored environment; must start with '/'" },
      name: { kind: 'string', help: "Display name; 1 to 255 characters after trimming" },
    },
  })

  endpoint(group, getBucketedStatus, {
    name: "get-bucketed-status",
    description: "Get bucketed status data for a monitor using TimescaleDB",
    path: { monitor_id: 'int' },
    query: {
      interval: { kind: 'string', help: "Bucket interval: '5min', 'hourly', or 'daily' (default: hourly)" },
      start_time: { kind: 'string', help: "Start time (ISO 8601) (default: 24 hours ago)" },
      end_time: { kind: 'string', help: "End time (ISO 8601) (default: now)" },
    },
  })

  endpoint(group, listCronMonitors, {
    name: "crons list",
    description: "List cron monitors",
    path: { project_id: 'project' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createCronMonitor, {
    name: "crons create",
    description: "Create cron monitor",
    path: { project_id: 'project' },
    body: {
      config: 'json!',
      environment_id: 'int',
      slug: 'string!',
    },
  })

  endpoint(group, deleteCronMonitor, {
    name: "crons delete",
    description: "Delete cron monitor",
    path: { project_id: 'project', monitor_id: 'int' },
    confirm: "This will delete cron monitor.",
  })

  endpoint(group, getCronMonitor, {
    name: "crons get",
    description: "Get cron monitor",
    path: { project_id: 'project', monitor_id: 'int' },
  })

  endpoint(group, updateCronMonitor, {
    name: "crons update",
    description: "Update cron monitor",
    path: { project_id: 'project', monitor_id: 'int' },
    body: {
      config: 'json',
      enabled: 'bool',
    },
  })

  endpoint(group, listCronMonitorRuns, {
    name: "crons list-runs",
    description: "List cron monitor runs",
    path: { project_id: 'project', monitor_id: 'int' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, detectEnvCredential, {
    name: "http-checks detect-env-credential",
    description: "Detect env credential",
    path: { project_id: 'project', env_var_id: 'int' },
  })

  endpoint(group, listVariableHistory, {
    name: "http-checks list-variable-history",
    description: "List variable history",
    path: { project_id: 'project', env_var_id: 'int' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, listHttpChecks, {
    name: "http-checks list",
    description: "List http checks",
    path: { project_id: 'project' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createHttpCheck, {
    name: "http-checks create",
    description: "Create http check",
    path: { project_id: 'project' },
    body: {
      credential: 'secret',
      enabled: 'bool',
      env_var_id: 'int',
      interval_seconds: 'int',
      kind: { kind: 'string', help: "'http' (default) calls an endpoint; 'local' reads expiring items (certificates, SSH certificates, OpenPGP...", values: ["http","local"] },
      local: 'json',
      name: 'string!',
      secret_id: { kind: 'int', help: "Project secret to check" },
      spec: { kind: 'json', help: "This is a declarative HTTP recipe, never executable code" },
    },
  })

  endpoint(group, getHttpChecksCapabilities, {
    name: "http-checks get-capabilities",
    description: "Get http checks capabilities",
    path: { project_id: 'project' },
  })

  endpoint(group, listHttpCheckPresets, {
    name: "http-checks list-presets",
    description: "List http check presets",
    path: { project_id: 'project' },
  })

  endpoint(group, deleteHttpCheck, {
    name: "http-checks delete",
    description: "Delete http check",
    path: { project_id: 'project', check_id: 'int' },
    confirm: "This will delete http check.",
  })

  endpoint(group, setHttpCheckEnabled, {
    name: "http-checks set-enabled",
    description: "Set http check enabled",
    path: { project_id: 'project', check_id: 'int' },
    body: {
      enabled: 'bool!',
    },
  })

  endpoint(group, updateHttpCheck, {
    name: "http-checks update",
    description: "Update http check",
    path: { project_id: 'project', check_id: 'int' },
    body: {
      credential: 'secret',
      enabled: 'bool',
      env_var_id: 'int',
      interval_seconds: 'int',
      kind: { kind: 'string', help: "'http' (default) calls an endpoint; 'local' reads expiring items (certificates, SSH certificates, OpenPGP...", values: ["http","local"] },
      local: 'json',
      name: 'string!',
      secret_id: { kind: 'int', help: "Project secret to check" },
      spec: { kind: 'json', help: "This is a declarative HTTP recipe, never executable code" },
    },
  })

  endpoint(group, runHttpCheck, {
    name: "http-checks run",
    description: "Run http check",
    path: { project_id: 'project', check_id: 'int' },
  })

  endpoint(group, detectSecretCredential, {
    name: "http-checks detect-secret-credential",
    description: "Detect secret credential",
    path: { project_id: 'project', secret_id: 'int' },
  })

  endpoint(group, listSecretHistory, {
    name: "http-checks list-secret-history",
    description: "List secret history",
    path: { project_id: 'project', secret_id: 'int' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })
}
