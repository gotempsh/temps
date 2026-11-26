// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps sandbox` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  accessCapability,
  approveSandboxService,
  approveServiceGrant,
  cmdKill,
  cmdLogs,
  createAccessService,
  createInternalAccessGrant,
  createInternalAccessSandboxGrant,
  deleteInternalAccessGrant,
  deleteInternalAccessSandboxGrant,
  getAccessService,
  getCmd,
  getSandboxCreation,
  getSandboxCreationBySandbox,
  getSandboxImagePreparation,
  getSandboxResourceDefaults,
  jobStatus,
  killJob,
  listAccessServices,
  listDir,
  listInternalAccessGrants,
  listInternalAccessSandboxGrants,
  listJobs,
  listSandboxInternalAddresses,
  listSandboxServices,
  listServiceGrants,
  prepareSandboxImages,
  requestSandboxService,
  requestServiceGrant,
  resizeSandbox,
  revokeSandboxService,
  revokeServiceGrant,
  rootfsGc,
  rootfsReport,
  sandboxCreatePreviewLink,
  sandboxIssueRuntimeEnvironment,
  updateAccessService,
  updateHostImagePreparation,
  writeFiles,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerSandboxEndpoints(program: Command): void {
  const group = topGroup(program, 'sandbox', "sandbox")

  endpoint(group, listAccessServices, {
    name: "service-access list",
    description: "List access services",
    path: { project_id: 'project' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createAccessService, {
    name: "service-access create",
    description: "Create access service",
    path: { project_id: 'project' },
    body: {
      clear_credential: 'bool',
      credential: { kind: 'secret', help: "A credential" },
      enabled: { kind: 'bool!', help: "Allows requests to be saved; does not enable a network connection" },
      expected_version: { kind: 'int', help: "Required for updates; omit for creation" },
      max_ttl_seconds: { kind: 'int!', help: "Maximum lifetime of a request, between 60 and 86400 seconds" },
      name: 'string!',
      policy: { kind: 'json!', help: "Exact destination and request scope" },
      vault_id: { kind: 'string', help: "Add a newly created service to this vault in the same transaction" },
    },
  })

  endpoint(group, getAccessService, {
    name: "service-access get",
    description: "Get access service",
    path: { project_id: 'project', service_id: 'string' },
  })

  endpoint(group, updateAccessService, {
    name: "service-access update",
    description: "Update access service",
    path: { project_id: 'project', service_id: 'string' },
    body: {
      clear_credential: 'bool',
      credential: { kind: 'secret', help: "A credential" },
      enabled: { kind: 'bool!', help: "Allows requests to be saved; does not enable a network connection" },
      expected_version: { kind: 'int', help: "Required for updates; omit for creation" },
      max_ttl_seconds: { kind: 'int!', help: "Maximum lifetime of a request, between 60 and 86400 seconds" },
      name: 'string!',
      policy: { kind: 'json!', help: "Exact destination and request scope" },
      vault_id: { kind: 'string', help: "Add a newly created service to this vault in the same transaction" },
    },
  })

  endpoint(group, listInternalAccessGrants, {
    name: "internal-access list-grants",
    description: "List the projects allowed to call this project's internal addresses",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Only grants that cover this environment of the project: its own and the project-wide ones" },
      page: { kind: 'int', help: "1-based page, default 1" },
      page_size: { kind: 'int', help: "Default 20, at most 100" },
    },
  })

  endpoint(group, createInternalAccessGrant, {
    name: "internal-access create-grant",
    description: "Allow another project to call this project's internal addresses",
    path: { project_id: 'project' },
    body: {
      caller_environment_id: { kind: 'int', help: "Only this environment of the calling project; omit for every environment, present and future" },
      caller_project_id: { kind: 'int!', help: "The project whose workloads may call" },
      description: { kind: 'string', help: "Why the grant exists, up to 200 characters" },
      target_environment_id: { kind: 'int', help: "The environment of this project that may be called; omit for every environment, present and future" },
    },
  })

  endpoint(group, listInternalAccessSandboxGrants, {
    name: "internal-access list-internal-access-sandbox-grants",
    description: "List the sandboxes allowed to call this project's internal addresses",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Only grants that cover this environment of the project: its own and the project-wide ones" },
      page: { kind: 'int', help: "1-based page, default 1" },
      page_size: { kind: 'int', help: "Default 20, at most 100" },
    },
  })

  endpoint(group, createInternalAccessSandboxGrant, {
    name: "internal-access create-internal-access-sandbox-grant",
    description: "Allow one of your sandboxes to call this project's internal addresses",
    path: { project_id: 'project' },
    body: {
      description: { kind: 'string', help: "Why the grant exists, up to 200 characters" },
      sandbox_id: { kind: 'string!', help: "Public id of one of your sandboxes" },
      target_environment_id: { kind: 'int', help: "The environment of this project that may be called; omit for every environment, present and future" },
    },
  })

  endpoint(group, deleteInternalAccessSandboxGrant, {
    name: "internal-access delete-grant",
    description: "Stop a sandbox from calling this project's internal addresses",
    path: { project_id: 'project', grant_id: 'int' },
    confirm: "This will stop a sandbox from calling this project's internal addresses.",
  })

  endpoint(group, deleteInternalAccessGrant, {
    name: "internal-access delete-internal-access-grant",
    description: "Stop another project from calling this project's internal addresses",
    path: { project_id: 'project', grant_id: 'int' },
    confirm: "This will stop another project from calling this project's internal addresses.",
  })

  endpoint(group, listServiceGrants, {
    name: "service-access list-grants",
    description: "List service grants",
    path: { project_id: 'project', sandbox_id: 'string' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, requestServiceGrant, {
    name: "service-access request-grant",
    description: "Request service grant",
    path: { project_id: 'project', sandbox_id: 'string' },
    body: {
      service_id: 'string!',
      service_version: 'int!',
      ttl_seconds: { kind: 'int!', help: "Absolute expiry is calculated at creation, even if the broker is unavailable" },
    },
  })

  endpoint(group, approveServiceGrant, {
    name: "service-access approve-grant",
    description: "Approve service grant",
    path: { project_id: 'project', sandbox_id: 'string' },
    body: {
      request_id: 'string!',
      service_id: 'string!',
      service_version: 'int!',
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, revokeServiceGrant, {
    name: "service-access revoke-grant",
    description: "Revoke service grant",
    path: { project_id: 'project', sandbox_id: 'string', grant_id: 'string' },
    body: {
      expected_version: 'int!',
    },
    confirm: "This will revoke service grant.",
  })

  endpoint(group, listSandboxServices, {
    name: "services list",
    description: "List sandbox services",
    path: { project_id: 'project', sandbox_id: 'string' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, requestSandboxService, {
    name: "services request",
    description: "Request sandbox service",
    path: { project_id: 'project', sandbox_id: 'string' },
    body: {
      service_id: 'string!',
      service_version: 'int!',
    },
  })

  endpoint(group, approveSandboxService, {
    name: "services approve",
    description: "Approve sandbox service",
    path: { project_id: 'project', sandbox_id: 'string', access_id: 'string' },
    body: {
      expected_version: 'int!',
      expires_at: { kind: 'string', help: "Omit for the sandbox lifetime" },
      service_version: 'int!',
    },
  })

  endpoint(group, revokeSandboxService, {
    name: "services revoke",
    description: "Revoke sandbox service",
    path: { project_id: 'project', sandbox_id: 'string', access_id: 'string' },
    body: {
      expected_version: 'int!',
    },
    confirm: "This will revoke sandbox service.",
  })

  endpoint(group, accessCapability, {
    name: "service-access capability",
    description: "Access capability",
    path: { project_id: 'project' },
  })

  endpoint(group, listSandboxInternalAddresses, {
    name: "internal-access list-addresses",
    description: "List the internal addresses one of your sandboxes may call",
    path: { sandbox_id: 'string' },
    query: {
      page: { kind: 'int', help: "1-based page, default 1" },
      page_size: { kind: 'int', help: "Default 20, at most 100" },
    },
  })

  endpoint(group, getSandboxCreation, {
    name: "get-creation",
    description: "Read a create's steps by the 'creation_id' its 'POST /v1/sandboxes' sent",
    path: { creation_id: 'string' },
  })

  endpoint(group, getSandboxImagePreparation, {
    name: "get-image-preparation",
    description: "Every host that can run sandboxes, with whether its default sandbox image is pulled (with download..",
  })

  endpoint(group, prepareSandboxImages, {
    name: "prepare-images",
    description: "Pull the default sandbox image on every eligible host (or one), so the first sandbox there starts without..",
    body: {
      node_id: { kind: 'int', help: "The host to prepare: a node id, or '0' for the control plane" },
    },
  })

  endpoint(group, updateHostImagePreparation, {
    name: "update-host-image-preparation",
    description: "Turn keeping one host's sandbox image prepared on or off",
    path: { node_id: 'int' },
    body: {
      keep_prepared: { kind: 'bool!', help: "Keep this host's default sandbox image pulled, re-pulling it when an upgrade changes the default. 'false'..." },
    },
  })

  endpoint(group, getSandboxResourceDefaults, {
    name: "get-resource-defaults",
    description: "Effective resource defaults and input ceilings for the requested placement",
    query: {
      node: 'string',
      backend: { kind: 'string', help: "Which isolation backend a sandbox runs on (ADR-029, ADR-050)", values: ["docker","firecracker","microsandbox","local"] },
    },
  })

  endpoint(group, rootfsReport, {
    name: "rootfs-report",
    description: "Inspect rootfs storage: the Firecracker digest-keyed cache (with which sandboxes reference each entry) and..",
  })

  endpoint(group, rootfsGc, {
    name: "rootfs-gc",
    description: "Reclaim rootfs cache entries not backing any live sandbox",
  })

  endpoint(group, getCmd, {
    name: "get-cmd",
    description: "Get cmd",
    path: { id: 'string', cmd_id: 'string' },
  })

  endpoint(group, cmdLogs, {
    name: "cmd-logs",
    description: "Stream a command's stdout/stderr as 'application/x-ndjson' ('@vercel/sandbox'-compatible)",
    path: { id: 'string', cmd_id: 'string' },
  })

  endpoint(group, getSandboxCreationBySandbox, {
    name: "get-creation-by",
    description: "Read the steps of the create that made a sandbox, while this server still tracks it (it is running, or..",
    path: { id: 'string' },
  })

  endpoint(group, listDir, {
    name: "list-dir",
    description: "List dir",
    path: { id: 'string' },
    query: {
      path: { kind: 'string!', help: "Absolute path of a directory inside the sandbox" },
      page: { kind: 'int', help: "1-based page (default 1)" },
      page_size: { kind: 'int', help: "Entries per page (default 20, max 100)" },
    },
  })

  endpoint(group, writeFiles, {
    name: "write-files",
    description: "Batch-write multiple files in a single request",
    path: { id: 'string' },
    body: {
      files: { kind: 'json!', help: "List of files to write" },
    },
  })

  endpoint(group, listJobs, {
    name: "list-jobs",
    description: "List jobs",
    path: { id: 'string' },
  })

  endpoint(group, jobStatus, {
    name: "job-status",
    description: "Job status",
    path: { id: 'string', job_id: 'string' },
  })

  endpoint(group, killJob, {
    name: "kill-job",
    description: "Terminate a detached job",
    path: { id: 'string', job_id: 'string' },
    body: {
      force: { kind: 'bool', help: "When true, sends SIGKILL immediately" },
    },
    confirm: "This will terminate a detached job.",
  })

  endpoint(group, sandboxCreatePreviewLink, {
    name: "create-preview-link",
    description: "Mint a shareable link to a sandbox preview",
    path: { id: 'string' },
    body: {
      path: { kind: 'string', help: "Path the recipient lands on" },
      port: { kind: 'int!', help: "Port inside the sandbox the preview serves on" },
      ttl_seconds: { kind: 'int', help: "How long the link stays usable, in seconds" },
    },
  })

  endpoint(group, resizeSandbox, {
    name: "resize",
    description: "Grow a Firecracker sandbox's root disk",
    path: { id: 'string' },
    body: {
      disk_size_mb: { kind: 'int!', help: "New root disk size in MB" },
    },
  })

  endpoint(group, sandboxIssueRuntimeEnvironment, {
    name: "issue-runtime-environment",
    description: "Issue the same scoped service variables used by a deployment runtime for the project attached to this sandbox",
    path: { id: 'string' },
  })

  endpoint(group, cmdKill, {
    name: "cmd-kill",
    description: "Kill a running command ('@vercel/sandbox'-compatible)",
    path: { id: 'string', cmd_id: 'string' },
    body: {
      force: { kind: 'bool', help: "Optional: when true, SIGKILL instead of SIGTERM" },
    },
  })
}
