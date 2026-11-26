// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps workflows` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  appGetWorkflowRun,
  appSendGrantedEvent,
  appSendWorkflowEvent,
  appStartGrantedWorkflow,
  appStartWorkflowRun,
  createWorkflowGrant,
  diagnoseWorkflowRun,
  getWorkflowCredential,
  getWorkflowSettings,
  issueWorkflowCredential,
  listAllWorkflowRuns,
  listWorkflowDefinitions,
  listWorkflowGrants,
  listWorkflowProjectKeys,
  listWorkflowSteps,
  revokeWorkflowCredential,
  revokeWorkflowGrant,
  shredWorkflowProjectKey,
  updateWorkflowSettings,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerWorkflowsEndpoints(program: Command): void {
  const group = topGroup(program, 'workflows', "workflows")

  endpoint(group, revokeWorkflowCredential, {
    name: "revoke-credential",
    description: "Revoke workflow credential",
    path: { project_id: 'project', environment_id: 'int' },
    confirm: "This will revoke workflow credential.",
  })

  endpoint(group, getWorkflowCredential, {
    name: "get-credential",
    description: "Get workflow credential",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, issueWorkflowCredential, {
    name: "issue-credential",
    description: "Issue workflow credential",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, listWorkflowGrants, {
    name: "list-grants",
    description: "List workflow grants",
    path: { project_id: 'project', environment_id: 'int' },
    query: {
      page: { kind: 'int', help: "1-indexed page number" },
      page_size: { kind: 'int', help: "Items per page" },
    },
  })

  endpoint(group, createWorkflowGrant, {
    name: "create-grant",
    description: "Create workflow grant",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      action: { kind: 'string!', values: ["start","event"] },
      resource: 'string!',
      source_environment_id: 'int!',
    },
  })

  endpoint(group, revokeWorkflowGrant, {
    name: "revoke-grant",
    description: "Revoke workflow grant",
    path: { project_id: 'project', environment_id: 'int', grant_id: 'string' },
    confirm: "This will revoke workflow grant.",
  })

  endpoint(group, diagnoseWorkflowRun, {
    name: "diagnose-run",
    description: "Diagnose workflow run",
    path: { project_id: 'project', environment_id: 'int', run_id: 'string' },
  })

  endpoint(group, listWorkflowSteps, {
    name: "list-steps",
    description: "List workflow steps",
    path: { project_id: 'project', environment_id: 'int', run_id: 'string' },
    query: {
      page: { kind: 'int', help: "1-indexed page number" },
      page_size: { kind: 'int', help: "Items per page" },
    },
  })

  endpoint(group, getWorkflowSettings, {
    name: "get-settings",
    description: "Get workflow settings",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, updateWorkflowSettings, {
    name: "update-settings",
    description: "Update workflow settings",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      drain_timeout_secs: { kind: 'int', help: "Opt-in deploy drain: pause new invocations and wait for running attempts" },
      host_service: { kind: 'string', help: "Compose service hosting the workflow handler; 'None': the main service" },
      inline_payload_limit_bytes: 'int!',
      invocation_deadline_secs: 'int!',
      lease_secs: 'int!',
      managed_store_service_id: { kind: 'int', help: "Managed PostgreSQL journal for new runs; existing runs keep their store" },
      max_active_runs: 'int!',
      max_concurrent_runs: 'int!',
      max_journal_bytes_per_run: 'int!',
      max_payload_bytes: 'int!',
      max_steps_per_run: 'int!',
      retention_days: 'int!',
      stuck_after_failures: 'int!',
    },
  })

  endpoint(group, listWorkflowProjectKeys, {
    name: "list-keys",
    description: "List workflow project keys",
    path: { project_id: 'project' },
    query: {
      page: { kind: 'int', help: "1-indexed page number" },
      page_size: { kind: 'int', help: "Items per page" },
    },
  })

  endpoint(group, shredWorkflowProjectKey, {
    name: "shred-key",
    description: "Shred workflow project key",
    path: { project_id: 'project', key_id: 'string' },
    body: {
      confirm_key_id: { kind: 'string!', help: "Exact key UUID, confirming that its retained payloads become unreadable" },
    },
  })

  endpoint(group, listWorkflowDefinitions, {
    name: "list-definitions",
    description: "List workflow definitions",
    query: {
      page: { kind: 'int', help: "1-based page; default 1" },
      page_size: { kind: 'int', help: "Default 20, at most 100" },
    },
  })

  endpoint(group, listAllWorkflowRuns, {
    name: "list-all-runs",
    description: "List all workflow runs",
    query: {
      status: { kind: 'string', help: "Where a run is in its lifecycle (ADR-106 §3)", values: ["queued","running","sleeping","waiting","retrying","completed","failed","cancelled","stuck"] },
      workflow: 'string',
      page_size: 'int',
      cursor: 'string',
    },
  })

  endpoint(group, appSendWorkflowEvent, {
    name: "app send-event",
    description: "App send workflow event",
    body: {
      key: { kind: 'string', help: "Narrows delivery to runs waiting on this key" },
      name: 'string!',
      payload: 'json',
    },
  })

  endpoint(group, appSendGrantedEvent, {
    name: "app send-granted-event",
    description: "App send granted event",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      key: { kind: 'string', help: "Narrows delivery to runs waiting on this key" },
      name: 'string!',
      payload: 'json',
    },
  })

  endpoint(group, appStartGrantedWorkflow, {
    name: "app start-granted",
    description: "App start granted workflow",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      idempotency_key: { kind: 'string', help: "A second start with the same key returns the first run" },
      input: 'json',
      workflow: 'string!',
    },
  })

  endpoint(group, appStartWorkflowRun, {
    name: "app start-run",
    description: "App start workflow run",
    body: {
      idempotency_key: { kind: 'string', help: "A second start with the same key returns the first run" },
      input: 'json',
      workflow: 'string!',
    },
  })

  endpoint(group, appGetWorkflowRun, {
    name: "app get-run",
    description: "App get workflow run",
    path: { run_id: 'string' },
  })
}
