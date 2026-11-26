// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps flags` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getFlagSnapshot,
  ofrepServerBulk,
  ofrepServerSingle,
  recordFlagExposure,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerFlagsEndpoints(program: Command): void {
  const group = topGroup(program, 'flags', "flags")

  endpoint(group, recordFlagExposure, {
    name: "record-exposure",
    description: "Record which flags a running app actually evaluated",
    body: {
      keys: { kind: 'strings!', help: "Flag keys evaluated since the last report" },
    },
  })

  endpoint(group, getFlagSnapshot, {
    name: "get-snapshot",
    description: "Every flag for the caller's environment, collapsed to what the evaluator needs",
    query: {
      environment_id: { kind: 'int', help: "Required only when the calling token is project-wide rather than scoped to a single environment" },
    },
  })

  endpoint(group, ofrepServerBulk, {
    name: "ofrep-server-bulk",
    description: "OFREP bulk evaluation for server-side OpenFeature SDKs",
    body: {
      context: 'json',
    },
  })

  endpoint(group, ofrepServerSingle, {
    name: "ofrep-server-single",
    description: "OFREP single-flag evaluation for server-side OpenFeature SDKs",
    path: { key: 'string' },
    body: {
      context: 'json',
    },
  })
}
