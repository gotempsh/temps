// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps cloud` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getCloudAiCapability,
  getCloudCapability,
  reconcileCloudBackupSource,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerCloudEndpoints(program: Command): void {
  const group = topGroup(program, 'cloud', "cloud")

  endpoint(group, getCloudAiCapability, {
    name: "get-ai-capability",
    description: "Get cloud ai capability",
  })

  endpoint(group, reconcileCloudBackupSource, {
    name: "reconcile-backup-source",
    description: "Reconcile cloud backup source",
  })

  endpoint(group, getCloudCapability, {
    name: "get-capability",
    description: "Get cloud capability",
  })
}
