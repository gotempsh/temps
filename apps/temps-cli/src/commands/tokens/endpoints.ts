// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps tokens` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  createDeploymentToken,
  deleteDeploymentToken,
  getDeploymentToken,
  listDeploymentTokens,
  rotateDeploymentToken,
  updateDeploymentToken,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerTokensEndpoints(program: Command): void {
  const group = topGroup(program, 'tokens', "tokens")

  endpoint(group, listDeploymentTokens, {
    name: "deployment list",
    description: "List all deployment tokens for a project",
    path: { project_id: 'project' },
    query: {
      page: { kind: 'int', help: "Page number (default: 1)" },
      page_size: { kind: 'int', help: "Page size (default: 20, max: 100)" },
    },
  })

  endpoint(group, createDeploymentToken, {
    name: "deployment create",
    description: "Create a new deployment token for a project",
    path: { project_id: 'project' },
    body: {
      deployment_id: { kind: 'int', help: "Optional deployment ID - if set, token is scoped to a specific deployment" },
      environment_id: { kind: 'int', help: "Optional environment ID - if not set, token applies to all environments" },
      expires_at: 'string',
      name: 'string!',
      permissions: { kind: 'strings', help: "List of permissions (e.g., [\"visitors:enrich\", \"emails:send\"]) If not provided, defaults to full access" },
    },
  })

  endpoint(group, deleteDeploymentToken, {
    name: "deployment delete",
    description: "Delete a deployment token",
    path: { project_id: 'project', token_id: 'int' },
    confirm: "This will delete a deployment token.",
  })

  endpoint(group, getDeploymentToken, {
    name: "deployment get",
    description: "Get a specific deployment token",
    path: { project_id: 'project', token_id: 'int' },
  })

  endpoint(group, updateDeploymentToken, {
    name: "deployment update",
    description: "Update a deployment token",
    path: { project_id: 'project', token_id: 'int' },
    body: {
      expires_at: 'string',
      is_active: 'bool',
      name: 'string',
      permissions: 'strings',
    },
  })

  endpoint(group, rotateDeploymentToken, {
    name: "deployment rotate",
    description: "Rotate a deployment token, invalidating its old secret and issuing a new one",
    path: { project_id: 'project', token_id: 'int' },
    confirm: "This will rotate a deployment token, invalidating its old secret and issuing a new one.",
  })
}
