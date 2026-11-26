// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps ip-access` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  checkAccessProvider,
  createAccessProvider,
  deleteAccessProvider,
  listAccessProviders,
  signEveryoneOutOfAddress,
  signOutOfApps,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerIpAccessEndpoints(program: Command): void {
  const group = topGroup(program, 'ip-access', "ip-access")

  endpoint(group, listAccessProviders, {
    name: "sessions list-providers",
    description: "The saved Cloudflare Access tokens",
  })

  endpoint(group, createAccessProvider, {
    name: "sessions create-provider",
    description: "Check a Cloudflare Access token, then save it sealed",
    body: {
      account_id: 'string!',
      name: { kind: 'string!', help: "A name to tell tokens apart" },
      token: { kind: 'secret!', help: "Checked, then sealed; never returned" },
    },
  })

  endpoint(group, checkAccessProvider, {
    name: "sessions check-provider",
    description: "Check which Cloudflare Access permissions a pasted token has: Access: Apps and Policies Edit (probed with..",
    body: {
      account_id: { kind: 'string!', help: "The Cloudflare account the Access applications live in (32 hex characters, from the dashboard URL or the..." },
      token: { kind: 'secret!', help: "The token, as pasted" },
    },
  })

  endpoint(group, deleteAccessProvider, {
    name: "sessions delete-provider",
    description: "Delete a saved Cloudflare Access token",
    path: { provider_id: 'int' },
    confirm: "This will delete a saved Cloudflare Access token.",
  })

  endpoint(group, signOutOfApps, {
    name: "sessions sign-out-of-apps",
    description: "Sign out of every signed-in address",
  })

  endpoint(group, signEveryoneOutOfAddress, {
    name: "sessions sign-everyone-out-of-address",
    description: "Sign everyone out of a signed-in address",
    path: { project_id: 'project', address_id: 'string' },
  })
}
