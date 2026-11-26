// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps apikeys` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getAiHarnessStatus,
  rotateApiKey,
  updateApiKey,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerApikeysEndpoints(program: Command): void {
  const group = topGroup(program, 'apikeys', "apikeys")

  endpoint(group, getAiHarnessStatus, {
    name: "get-ai-harness-status",
    description: "Whether the caller's external AI harness has connected",
  })

  endpoint(group, updateApiKey, {
    name: "update-api-key",
    description: "Update api key",
    path: { id: 'int' },
    body: {
      expires_at: 'string',
      is_active: 'bool',
      name: 'string',
      permissions: 'strings',
    },
  })

  endpoint(group, rotateApiKey, {
    name: "rotate-api-key",
    description: "Rotate api key",
    path: { id: 'int' },
    confirm: "This will rotate api key.",
  })
}
