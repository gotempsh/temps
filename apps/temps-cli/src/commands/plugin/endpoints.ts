// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps plugin` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  changeChatPluginWatch,
  getPluginAiTools,
  listChatPluginWatches,
  putPluginAiTools,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerPluginEndpoints(program: Command): void {
  const group = topGroup(program, 'plugin', "plugin")

  endpoint(group, listChatPluginWatches, {
    name: "api list-chat-watches",
    description: "List chat plugin watches",
    path: { chat: 'string' },
  })

  endpoint(group, changeChatPluginWatch, {
    name: "api change-chat-watch",
    description: "Change chat plugin watch",
    path: { chat: 'string' },
    body: {
      action: { kind: 'string!', values: ["create","pause","resume","remove"] },
      digest: 'string',
      mode: { kind: 'string', values: ["notify","wake"] },
      plugin: 'string',
      source: 'string',
      subject: 'string',
      where: 'json',
      id: 'string',
    },
  })

  endpoint(group, getPluginAiTools, {
    name: "api get-ai-tools",
    description: "Get plugin ai tools",
    path: { name: 'string' },
  })

  endpoint(group, putPluginAiTools, {
    name: "api put-ai-tools",
    description: "Put plugin ai tools",
    path: { name: 'string' },
    body: {
      actor_id: { kind: 'string!', help: "Current actor_id returned by GET; replacing the installation requires fresh consent" },
      enabled: 'bool!',
      installation_revision: { kind: 'string!', help: "Binary revision returned by GET; a changed binary requires fresh consent" },
    },
  })
}
