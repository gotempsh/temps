// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps network-policy` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  listNodeModes,
  setNodeMode,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerNetworkPolicyEndpoints(program: Command): void {
  const group = topGroup(program, 'network-policy', "network-policy")

  endpoint(group, listNodeModes, {
    name: "list-node-modes",
    description: "List node modes",
  })

  endpoint(group, setNodeMode, {
    name: "set-node-mode",
    description: "Set node mode",
    path: { node_id: 'int' },
    body: {
      mode: 'string!',
    },
  })
}
