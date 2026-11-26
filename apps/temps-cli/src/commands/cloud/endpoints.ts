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
  updateCloudFeatures,
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

  endpoint(group, updateCloudFeatures, {
    name: "features",
    description: "Turn the Temps Cloud features this instance uses on or off; omitted ones keep their setting",
    body: {
      backups_enabled: { kind: 'bool', help: "Off-site backups through Temps Cloud" },
      console_access_enabled: { kind: 'bool', help: "Let Temps Cloud open this console (see `temps cloud console-access`)" },
      notifications_enabled: { kind: 'bool', help: "Notifications delivered through Temps Cloud" },
      telemetry_enabled: { kind: 'bool', help: "Cloud telemetry for spans, analytics and metrics" },
    },
  })

  endpoint(group, getCloudCapability, {
    name: "get-capability",
    description: "Get cloud capability",
  })
}
