// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps projects` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  addEnvironmentDomain,
  createProjectFromTemplate,
  deleteEnvironmentDomain,
  dismissEnvironmentVariableSecretSuggestion,
  getComposeSecurity,
  getEnvironment,
  getEnvironmentDomains,
  getEnvironmentVariable,
  getProjectIngestSetup,
  getProjectServiceTemplate,
  getProjectStatistics,
  getResolvedEnvironmentVariableValue,
  getResolvedEnvironmentVariables,
  reinstallGithubWebhook,
  reinstallGitlabWebhook,
  teardownEnvironment,
  updateAutomaticDeploy,
  updateComposeSecurity,
  updateEnvironmentSubdomain,
  updateServiceTemplateRuntime,
  upgradeProjectServiceTemplate,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerProjectsEndpoints(program: Command): void {
  const group = topGroup(program, 'projects', "projects")

  endpoint(group, createProjectFromTemplate, {
    name: "create-from-template",
    description: "Create a new project from a template",
    body: {
      automatic_deploy: { kind: 'bool', help: "Enable automatic deployment on push (defaults to true)" },
      cloudflare_enabled: { kind: 'bool', help: "Override the Cloudflare default for this new project" },
      command: { kind: 'strings', help: "Optional image entrypoint arguments" },
      cpu_limit: { kind: 'int', help: "CPU limit override in microcores" },
      cpu_request: { kind: 'int', help: "CPU request override in microcores (1_000_000 = one CPU core)" },
      delivery_provider: 'string',
      environment_variables: { kind: 'json', help: "Environment variables to set (key-value pairs)" },
      exposed_port: { kind: 'int', help: "Public container port override" },
      git_provider_connection_id: { kind: 'int', help: "Git provider connection ID" },
      health_check_path: { kind: 'string', help: "Relative HTTP health-check path override" },
      image: { kind: 'string', help: "Optional prebuilt-image override" },
      memory_limit: { kind: 'int', help: "Memory limit override in MiB" },
      memory_request: { kind: 'int', help: "Memory request override in MiB" },
      private: { kind: 'bool', help: "Whether to make the repository private (defaults to true)" },
      project_name: { kind: 'string!', help: "Name for the new project" },
      repository_name: { kind: 'string', help: "Name for the new repository to create" },
      repository_owner: { kind: 'string', help: "Owner/organization for the new repository (defaults to authenticated user)" },
      storage_service_ids: { kind: 'ints', help: "External storage service IDs to attach to the project" },
      template_slug: { kind: 'string!', help: "Template slug to use as the base" },
    },
  })

  endpoint(group, getProjectStatistics, {
    name: "get-statistics",
    description: "Get project statistics",
  })

  endpoint(group, getComposeSecurity, {
    name: "get-compose-security",
    description: "Get compose security",
    path: { id: 'int' },
  })

  endpoint(group, updateComposeSecurity, {
    name: "update-compose-security",
    description: "Update compose security",
    path: { id: 'int' },
    body: {
      acknowledge_legacy_migration: 'bool',
      acknowledge_risks: 'bool',
      expected_policy: { kind: 'json!', help: "Policy read by the editor; compared while holding the project lock" },
      policy: 'json!',
    },
  })

  endpoint(group, updateAutomaticDeploy, {
    name: "update-automatic-deploy",
    description: "Update automatic deployment setting for a project",
    path: { project_id: 'project' },
    body: {
      automatic_deploy: 'bool!',
    },
  })

  endpoint(group, getResolvedEnvironmentVariables, {
    name: "get-resolved-environment-variables",
    description: "Resolved env vars for a project (manual + integration-sourced, merged)",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Optional environment ID to filter manual vars by" },
    },
  })

  endpoint(group, getResolvedEnvironmentVariableValue, {
    name: "get-resolved-environment-variable-value",
    description: "Reveal the plaintext value of a resolved environment variable",
    path: { project_id: 'project', key: 'string' },
    query: {
      environment_id: { kind: 'int', help: "Optional environment ID" },
      var_id: { kind: 'int', help: "Exact manual environment-variable row ID" },
      service_id: { kind: 'int', help: "Integration service ID shown by the resolved list" },
    },
  })

  endpoint(group, getEnvironmentVariable, {
    name: "get-environment-variable",
    description: "Get one environment variable of a project by its ID",
    path: { project_id: 'project', var_id: 'int' },
  })

  endpoint(group, dismissEnvironmentVariableSecretSuggestion, {
    name: "dismiss-environment-variable-secret-suggestion",
    description: "Dismiss the \"store as secret\" suggestion for a regular variable",
    path: { project_id: 'project', var_id: 'int' },
    confirm: "This will dismiss the \"store as secret\" suggestion for a regular variable.",
  })

  endpoint(group, getEnvironment, {
    name: "get-environment",
    description: "Get a specific environment by ID or slug",
    path: { project_id: 'project', env_id: 'int' },
  })

  endpoint(group, getEnvironmentDomains, {
    name: "get-environment-domains",
    description: "Get all environment domains for a specific environment",
    path: { project_id: 'project', env_id: 'int' },
  })

  endpoint(group, addEnvironmentDomain, {
    name: "add-environment-domain",
    description: "Add a new environment domain",
    path: { project_id: 'project', env_id: 'int' },
    body: {
      domain: 'string!',
      is_primary: 'bool!',
    },
  })

  endpoint(group, deleteEnvironmentDomain, {
    name: "delete-environment-domain",
    description: "Delete an environment domain",
    path: { project_id: 'project', env_id: 'int', domain_id: 'int' },
    confirm: "This will delete an environment domain.",
  })

  endpoint(group, updateEnvironmentSubdomain, {
    name: "update-environment-subdomain",
    description: "Rename the auto-managed subdomain for an environment",
    path: { project_id: 'project', env_id: 'int' },
    body: {
      subdomain: { kind: 'string!', help: "New subdomain label" },
    },
  })

  endpoint(group, teardownEnvironment, {
    name: "teardown-environment",
    description: "Teardown an environment and all its active deployments",
    path: { project_id: 'project', env_id: 'int' },
    confirm: "This will teardown an environment and all its active deployments.",
  })

  endpoint(group, reinstallGithubWebhook, {
    name: "reinstall-github-webhook",
    description: "Install or reinstall the GitHub repository webhook for a project",
    path: { project_id: 'project' },
  })

  endpoint(group, reinstallGitlabWebhook, {
    name: "reinstall-gitlab-webhook",
    description: "Reinstall the GitLab webhook for a project",
    path: { project_id: 'project' },
  })

  endpoint(group, getProjectIngestSetup, {
    name: "get-ingest-setup",
    description: "Get where this project's apps send telemetry",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Attribute telemetry to this environment of the project" },
    },
  })

  endpoint(group, updateServiceTemplateRuntime, {
    name: "update-service-template-runtime",
    description: "Update service template runtime",
    path: { project_id: 'project' },
    body: {
      command: { kind: 'strings', help: "Empty means use the image's own default command" },
      cpuLimit: 'int',
      cpuRequest: 'int',
      exposedPort: 'int',
      healthCheckPath: 'string!',
      imageRef: 'string!',
      memoryLimit: 'int',
      memoryRequest: 'int',
    },
  })

  endpoint(group, getProjectServiceTemplate, {
    name: "get-service-template",
    description: "Return the immutable service-template release applied to a project together with catalog drift, missing..",
    path: { project_id: 'project' },
  })

  endpoint(group, upgradeProjectServiceTemplate, {
    name: "upgrade-service-template",
    description: "Upgrade project service template",
    path: { project_id: 'project' },
    body: {
      environment_variables: { kind: 'json', help: "Values for inputs introduced by the target release" },
      target_version: { kind: 'string!', help: "Optimistic target selected from the preview" },
    },
  })
}
