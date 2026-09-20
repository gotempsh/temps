// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, keyValue } from '../../ui/output.js'

// ============================================================================
// Hand-written request/response shapes
// ============================================================================
//
// `GET /api/nodes/capability` is implemented in `temps-deployments` (core, not
// a plugin) so it belongs in the generated OpenAPI client in principle.
// Generating requires `bun run spec:update` against a live server, which
// wasn't available when this command was added — see root CLAUDE.md's
// "Regenerating the OpenAPI clients". This interface is hand-maintained to
// mirror `crates/temps-deployments/src/handlers/nodes.rs`'s
// `NodeCapabilityResponse` exactly. Once the spec is regenerated against a
// running server, switch this command to the generated types/functions and
// delete this. Same arrangement as `commands/cluster/index.ts`.

export interface NodeCapabilityResponse {
  /** Whether the control plane itself may run containers, builds and services. */
  local_workloads: boolean
  /** Active, heartbeating worker nodes. Excludes the control plane. */
  active_worker_nodes: number
  /** Whether a workload can be placed at all. */
  schedulable: boolean
  /** Why nothing can be placed, when `schedulable` is false. */
  reason: string | null
  /** Console path where an operator joins a worker node. */
  setup_path: string
}

// ============================================================================
// Presentation (unit tested)
// ============================================================================

/** Where workloads can run, in one line, for the summary row. */
export function describeCapability(capability: NodeCapabilityResponse): string {
  if (!capability.schedulable) return 'nowhere — no schedulable target'
  const targets: string[] = []
  if (capability.local_workloads) targets.push('this host')
  if (capability.active_worker_nodes > 0) {
    targets.push(
      capability.active_worker_nodes === 1
        ? '1 worker node'
        : `${capability.active_worker_nodes} worker nodes`
    )
  }
  return targets.join(' + ')
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerNodesCommands(program: Command): void {
  const nodes = program.command('nodes').description('Worker nodes and workload placement')

  nodes
    .command('capability')
    .description(
      'Show whether this install can run workloads at all — local workloads plus joined ' +
        'worker nodes — so a deploy that could never be scheduled is visible before it is queued'
    )
    .option('--json', 'Output in JSON format')
    .action(capabilityAction)
}

// ============================================================================
// Actions
// ============================================================================

async function capabilityAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Checking workload placement capability...', async () => {
    const { data, error } = await client.get<NodeCapabilityResponse, ProblemDetails>({
      url: 'nodes/capability',
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.globe} Workload Placement`)
  keyValue(
    'Local workloads',
    result.local_workloads ? colors.success('enabled') : colors.muted('disabled')
  )
  keyValue('Active worker nodes', result.active_worker_nodes)
  keyValue(
    'Schedulable',
    result.schedulable ? colors.success('yes') : colors.error('no')
  )
  keyValue('Workloads run on', describeCapability(result))

  if (!result.schedulable) {
    newline()
    console.log(`  ${colors.error(result.reason ?? 'Nothing can be scheduled.')}`)
    console.log(
      `  ${colors.muted(`Join a worker node with \`temps join\`, or configure one at ${result.setup_path}`)}`
    )
  }

  newline()
}
