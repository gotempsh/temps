// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps funnels` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getUniqueEvents,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerFunnelsEndpoints(program: Command): void {
  const group = topGroup(program, 'funnels', "funnels")

  endpoint(group, getUniqueEvents, {
    name: "get-unique-events",
    description: "Get all unique/distinct event types for a project (paginated)",
    path: { project_id: 'project' },
    query: {
      page: { kind: 'int', help: "Page number (default: 1)" },
      page_size: { kind: 'int', help: "Items per page (default: 50, max: 100)" },
    },
  })
}
