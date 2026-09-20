// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import { nodeCapabilityGet } from '../../api/sdk.gen.js'
import type { NodeCapabilityResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, keyValue } from '../../ui/output.js'

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

/**
 * What to do about an unschedulable install, addressed to whoever is holding
 * this credential.
 *
 * A token without Settings permissions cannot mint an enrollment token, so
 * telling its owner to "configure one at /settings/nodes" sends them to a page
 * that refuses them. Say who to ask instead — the operator running this CLI
 * has no support channel to work that out from.
 */
export function capabilityRemedy(capability: NodeCapabilityResponse): string {
  if (!capability.can_manage_nodes) {
    return 'Ask an administrator to add a worker node — this credential cannot manage nodes.'
  }
  return `Join a worker node with \`temps join\`, or configure one at ${capability.setup_path}`
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
    const { data, error } = await nodeCapabilityGet()
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
    console.log(`  ${colors.muted(capabilityRemedy(result))}`)
  }

  newline()
}
