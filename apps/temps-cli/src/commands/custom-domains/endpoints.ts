// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps custom-domains` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getVisibleCustomDomainByHostname,
  reassignProjectCustomDomain,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerCustomDomainsEndpoints(program: Command): void {
  const group = topGroup(program, 'custom-domains', "custom-domains")

  endpoint(group, getVisibleCustomDomainByHostname, {
    name: "get-visible-by-hostname",
    description: "Find the project assignment for a certificate hostname",
    path: { hostname: 'string' },
  })

  endpoint(group, reassignProjectCustomDomain, {
    name: "reassign",
    description: "Move a domain between projects without deleting its route or certificate",
    path: { source_project_id: 'int', domain_id: 'int' },
    body: {
      target_environment_id: 'int!',
      target_project_id: 'int!',
    },
  })
}
