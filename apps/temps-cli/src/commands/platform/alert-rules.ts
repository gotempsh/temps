// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Platform-level (node-scoped) monitoring alert rules.
 *
 * These are the rules the control-plane seeds for itself on startup — proxy
 * error rate / latency and file-descriptor (socket) exhaustion. They live on
 * the synthetic node 0 rather than on any project or service, so none of the
 * project-scoped alert commands can reach them.
 *
 * Update-only by design: the defaults are re-seeded on every server start
 * (`ON CONFLICT DO NOTHING`), so deleting a rule would silently bring it back
 * on the next restart. `--disable` is the operation that actually sticks.
 */
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { nodeMetricsGetAlertRules, nodeMetricsUpdateAlertRule } from '../../api/sdk.gen.js'
import type { ServiceUpdateAlertRuleRequest } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { header, newline, json as jsonOut, colors, keyValue, success } from '../../ui/output.js'

/** The control-plane is always node 0 — see `CONTROL_PLANE_NODE_ID` in temps-metrics. */
const CONTROL_PLANE_NODE_ID = 0

interface ListOptions {
  node?: string
  json?: boolean
}

interface SetOptions {
  node?: string
  threshold?: string
  comparator?: string
  severity?: string
  forDuration?: string
  enable?: boolean
  disable?: boolean
  json?: boolean
}

function parseNodeId(raw: string | undefined): number {
  if (raw === undefined) return CONTROL_PLANE_NODE_ID
  const id = Number(raw)
  if (!Number.isInteger(id) || id < 0) {
    throw new Error(`Invalid --node "${raw}": expected a non-negative integer node ID`)
  }
  return id
}

export async function platformAlertRulesList(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const nodeId = parseNodeId(options.node)

  const rules = await withSpinner('Fetching node alert rules...', async () => {
    const { data, error } = await nodeMetricsGetAlertRules({ client, path: { id: nodeId } })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  if (options.json) {
    jsonOut({ node_id: nodeId, rules })
    return
  }

  header(`Platform Alert Rules — node ${nodeId}`)
  newline()

  if (rules.length === 0) {
    console.log(
      colors.muted(
        '  No alert rules on this node. The control-plane defaults are seeded on server startup.'
      )
    )
    newline()
    return
  }

  for (const rule of rules) {
    const state = rule.enabled ? colors.success('enabled') : colors.muted('disabled')
    console.log(`  ${colors.bold(String(rule.id))}  ${rule.name}  ${state}`)
    console.log(
      colors.muted(
        `      ${rule.metric_name} ${rule.comparator} ${rule.threshold} ` +
          `for ${rule.for_duration_secs}s → ${rule.severity}`
      )
    )
    if (rule.silenced_until) {
      console.log(colors.muted(`      silenced until ${rule.silenced_until}`))
    }
  }

  newline()
  console.log(
    colors.muted(
      `  ${rules.length} rule(s). Retune one with: ` +
        `temps platform alert-rules set <rule-id> --threshold <n>`
    )
  )
  newline()
}

export async function platformAlertRulesSet(ruleId: string, options: SetOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const nodeId = parseNodeId(options.node)

  const id = Number(ruleId)
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`Invalid rule ID "${ruleId}": expected a positive integer`)
  }

  if (options.enable && options.disable) {
    throw new Error('--enable and --disable are mutually exclusive')
  }

  const body: ServiceUpdateAlertRuleRequest = {}

  if (options.threshold !== undefined) {
    const threshold = Number(options.threshold)
    if (!Number.isFinite(threshold)) {
      throw new Error(`Invalid --threshold "${options.threshold}": expected a number`)
    }
    body.threshold = threshold
  }
  if (options.comparator !== undefined) body.comparator = options.comparator
  if (options.severity !== undefined) body.severity = options.severity
  if (options.forDuration !== undefined) {
    const forDuration = Number(options.forDuration)
    if (!Number.isInteger(forDuration) || forDuration < 0) {
      throw new Error(
        `Invalid --for-duration "${options.forDuration}": expected a non-negative integer (seconds)`
      )
    }
    body.for_duration_secs = forDuration
  }
  if (options.enable) body.enabled = true
  if (options.disable) body.enabled = false

  if (Object.keys(body).length === 0) {
    throw new Error(
      'Nothing to update. Pass at least one of --threshold, --comparator, --severity, ' +
        '--for-duration, --enable, --disable'
    )
  }

  const updated = await withSpinner(`Updating alert rule ${id}...`, async () => {
    const { data, error } = await nodeMetricsUpdateAlertRule({
      client,
      path: { id: nodeId, rule_id: id },
      body,
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error(`Alert rule ${id} not found on node ${nodeId}`)
    return data
  })

  if (options.json) {
    jsonOut(updated)
    return
  }

  success(`Alert rule ${updated.id} updated`)
  newline()
  keyValue('Name', updated.name)
  keyValue('Metric', updated.metric_name)
  keyValue('Condition', `${updated.comparator} ${updated.threshold}`)
  keyValue('For', `${updated.for_duration_secs}s`)
  keyValue('Severity', updated.severity)
  keyValue('Enabled', updated.enabled)
  newline()
}
