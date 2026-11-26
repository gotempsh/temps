// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Host metrics beyond one node: a metric combined across nodes, which
 * workers report metrics, drive health, and resource alerts on a node.
 */

import type { Command } from 'commander'
import { client } from '../../lib/api-client.js'
import {
  nodeMetricsCreateAlertRule,
  nodeMetricsDeleteAlertRule,
  nodeMetricsGetClusterLatest,
  nodeMetricsGetClusterRange,
  nodeMetricsGetDiskHealth,
  nodeMetricsGetReporting,
} from '../../api/sdk.gen.js'
import type { NodeCreateAlertRuleRequest } from '../../api/types.gen.js'
import { call, confirmRisk, connect, output, parseIntegerFlag, type JsonOption } from '../../lib/command-kit.js'
import { ValidationError } from '../../utils/errors.js'

/** `--start`/`--end`/`--percentile` as metric query fields. */
export function metricWindowQuery(flags: { start?: string; end?: string; percentile?: string }): {
  start_time?: string
  end_time?: string
  percentile?: number
} {
  for (const [flag, value] of [['--start', flags.start], ['--end', flags.end]] as const) {
    if (value !== undefined && Number.isNaN(Date.parse(value))) {
      throw new ValidationError(`${flag} must be an ISO 8601 time, e.g. 2026-01-01T00:00:00Z, got "${value}"`)
    }
  }
  let percentile: number | undefined
  if (flags.percentile !== undefined) {
    percentile = Number(flags.percentile)
    if (!Number.isFinite(percentile) || percentile <= 0 || percentile > 1) {
      throw new ValidationError(`--percentile must be a fraction in (0, 1], e.g. 0.95, got "${flags.percentile}"`)
    }
    // The API takes the percentile in 0-100 units; round away float noise (0.29 * 100 = 28.999...).
    percentile = Number((percentile * 100).toFixed(6))
  }
  return {
    ...(flags.start !== undefined && { start_time: flags.start }),
    ...(flags.end !== undefined && { end_time: flags.end }),
    ...(percentile !== undefined && { percentile }),
  }
}

/** `--nodes 1,2,3` as the comma-separated ID list the API takes. */
export function nodeIdList(raw: string): string {
  const ids = raw.split(',').map((part) => parseIntegerFlag(part, '--nodes', 0))
  if (ids.length === 0) throw new ValidationError('--nodes needs at least one node ID')
  return ids.join(',')
}

/** The API takes the operator itself; the words are accepted too, since `>` needs quoting in a shell. */
const COMPARATORS = ['>', '<', '>=', '<=']
const COMPARATOR_WORDS: Record<string, string> = { gt: '>', lt: '<', gte: '>=', lte: '<=' }
const SEVERITIES = ['warning', 'critical']

export function alertRuleRequest(flags: {
  name: string
  metric: string
  comparator: string
  threshold: string
  severity: string
  for?: string
  disabled?: boolean
}): NodeCreateAlertRuleRequest {
  const comparator = COMPARATOR_WORDS[flags.comparator] ?? flags.comparator
  if (!COMPARATORS.includes(comparator)) {
    throw new ValidationError(`--comparator must be one of ${COMPARATORS.join(', ')} (or gt, lt, gte, lte), got "${flags.comparator}"`)
  }
  if (!SEVERITIES.includes(flags.severity)) {
    throw new ValidationError(`--severity must be one of ${SEVERITIES.join(', ')}, got "${flags.severity}"`)
  }
  const threshold = Number(flags.threshold)
  if (!Number.isFinite(threshold)) throw new ValidationError(`--threshold must be a number, got "${flags.threshold}"`)
  return {
    name: flags.name,
    metric_name: flags.metric,
    comparator,
    threshold,
    severity: flags.severity,
    ...(flags.for !== undefined && { for_duration_secs: parseIntegerFlag(flags.for, '--for', 0) }),
    ...(flags.disabled && { enabled: false }),
  }
}

export function registerServerExtraCommands(server: Command): void {
  server
    .command('cluster-metrics')
    .description('Time series of one metric combined across nodes (counters summed, ratios averaged)')
    .requiredOption('--metric <name>', 'Metric name, e.g. node.cpu_percent')
    .requiredOption('--nodes <ids>', 'Comma-separated node IDs, e.g. 0,1,2')
    .option('--range <range>', 'Time window, e.g. 1h, 24h, 7d')
    .option('--start <time>', 'Start of an explicit window (ISO 8601)')
    .option('--end <time>', 'End of an explicit window (ISO 8601)')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { metric: string; nodes: string; range?: string; start?: string; end?: string }) => {
      const { percentile: _unused, ...window } = metricWindowQuery(options)
      const query = { metric: options.metric, node_ids: nodeIdList(options.nodes), ...(options.range !== undefined && { range: options.range }), ...window }
      await connect()
      output(await call(`Could not read ${options.metric} across nodes`, () => nodeMetricsGetClusterRange({ client, query })), options, options.metric)
    })

  server
    .command('cluster-latest')
    .description('Latest value of every metric, combined across nodes')
    .requiredOption('--nodes <ids>', 'Comma-separated node IDs')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { nodes: string }) => {
      const query = { node_ids: nodeIdList(options.nodes) }
      await connect()
      output(await call('Could not read the latest cluster metrics', () => nodeMetricsGetClusterLatest({ client, query })), options, 'Latest cluster metrics')
    })

  server
    .command('reporting')
    .description('Whether each worker reports metrics, and why not')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption) => {
      await connect()
      output(await call('Could not read metrics reporting', () => nodeMetricsGetReporting({ client })), options, 'Metrics reporting')
    })

  server
    .command('disk-health')
    .description("Latest software RAID and drive SMART health for a node")
    .option('--node <id>', 'Node ID (0 = control plane)', '0')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { node: string }) => {
      const id = parseIntegerFlag(options.node, '--node', 0)
      await connect()
      output(await call(`Could not read the disk health of node ${id}`, () => nodeMetricsGetDiskHealth({ client, path: { id } })), options, `Disk health of node ${id}`)
    })

  const alerts = server.command('alert-rules').description('CPU, memory and disk alerts on a node')
  alerts
    .command('create')
    .description('Alert when a node metric crosses a threshold')
    .option('--node <id>', 'Node ID (0 = control plane)', '0')
    .requiredOption('--name <name>', 'Rule name')
    .requiredOption('--metric <name>', 'Metric, e.g. node.cpu_percent or node.disk_used_percent')
    .requiredOption('--comparator <op>', `One of ${COMPARATORS.join(', ')}, or gt, lt, gte, lte`)
    .requiredOption('--threshold <value>', 'Threshold value')
    .option('--severity <level>', `One of ${SEVERITIES.join(', ')}`, 'warning')
    .option('--for <seconds>', 'How long the condition must hold before it fires')
    .option('--disabled', 'Create it turned off')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { node: string; name: string; metric: string; comparator: string; threshold: string; severity: string; for?: string; disabled?: boolean }) => {
      const id = parseIntegerFlag(options.node, '--node', 0)
      const body = alertRuleRequest(options)
      await connect()
      output(await call(`Could not create the alert on node ${id}`, () => nodeMetricsCreateAlertRule({ client, path: { id }, body })), options, undefined, `Alert "${options.name}" created`)
    })
  alerts
    .command('delete <rule-id>')
    .alias('rm')
    .description("Delete a node's resource alert")
    .option('--node <id>', 'Node ID (0 = control plane)', '0')
    .option('-y, --yes', 'Do not ask for confirmation')
    .action(async (rule: string, options: { node: string; yes?: boolean }) => {
      const id = parseIntegerFlag(options.node, '--node', 0)
      const ruleId = parseIntegerFlag(rule, '<rule-id>', 1)
      await connect()
      if (!(await confirmRisk([`Alert rule ${ruleId} on node ${id} stops firing.`], 'Delete it?', options.yes))) return
      await call(`Could not delete alert rule ${ruleId}`, () => nodeMetricsDeleteAlertRule({ client, path: { id, rule_id: ruleId } }))
      output(undefined, {}, undefined, `Alert rule ${ruleId} deleted`)
    })
}
