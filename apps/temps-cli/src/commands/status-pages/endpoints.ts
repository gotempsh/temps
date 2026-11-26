// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps status-pages` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getPublicStatusPageFeed,
  getPublicStatusPageJson,
  getStatusOverview,
  getStatusPageMaintenance,
  previewStatusPage,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerStatusPagesEndpoints(program: Command): void {
  const group = topGroup(program, 'status-pages', "status-pages")

  endpoint(group, getStatusOverview, {
    name: "get-overview",
    description: "Get status page overview",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Filter by environment ID" },
    },
  })

  endpoint(group, getStatusPageMaintenance, {
    name: "get-maintenance",
    description: "A maintenance window",
    path: { status_page_id: 'int', maintenance_id: 'int' },
  })

  endpoint(group, previewStatusPage, {
    name: "preview",
    description: "Preview what a status page shows visitors, published or not",
    path: { status_page_id: 'int' },
  })

  endpoint(group, getPublicStatusPageFeed, {
    name: "get-public-feed",
    description: "Public status page (Atom feed)",
    path: { slug: 'string' },
    output: 'bytes',
  })

  endpoint(group, getPublicStatusPageJson, {
    name: "get-public-json",
    description: "Public status page (JSON)",
    path: { slug: 'string' },
  })
}
