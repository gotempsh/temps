// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps templates` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  listProjectTemplateTags,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerTemplatesEndpoints(program: Command): void {
  const group = topGroup(program, 'templates', "templates")

  endpoint(group, listProjectTemplateTags, {
    name: "list-tags",
    description: "List all available template tags",
  })
}
