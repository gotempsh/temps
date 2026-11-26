// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps incidents` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  updateIncident,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerIncidentsEndpoints(program: Command): void {
  const group = topGroup(program, 'incidents', "incidents")

  endpoint(group, updateIncident, {
    name: "update",
    description: "Set or clear the heading an incident shows on published status pages",
    path: { incident_id: 'int' },
    body: {
      public_title: { kind: 'string!', help: "New public heading" },
    },
  })
}
