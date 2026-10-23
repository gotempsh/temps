// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Alert rules on a node's resource metrics (CPU, memory, disk).
 *
 * The control plane (node 0) and every worker record the same three
 * percentage gauges, so the same rules apply to both. The allowlist and the
 * bounds mirror the server's validation (`NodeAlertRuleService`), which stays
 * authoritative; checking here only saves the operator a round trip.
 */
import type { NodeCreateAlertRuleRequest } from '@/api/client/types.gen'
import { z } from 'zod'

export const ALERTABLE_NODE_METRICS = [
  'node.cpu_percent',
  'node.memory_percent',
  'node.disk_percent',
] as const

export type AlertableNodeMetric = (typeof ALERTABLE_NODE_METRICS)[number]

export const NODE_ALERT_METRIC_LABELS: Record<AlertableNodeMetric, string> = {
  'node.cpu_percent': 'CPU usage',
  'node.memory_percent': 'Memory usage',
  'node.disk_percent': 'Disk usage',
}

export const NODE_ALERT_COMPARATORS = ['>', '>=', '<', '<='] as const
export const NODE_ALERT_SEVERITIES = ['warning', 'critical'] as const

/** Longest breach the server accepts, in seconds (one day). */
export const MAX_FOR_DURATION_SECS = 86_400
export const MAX_RULE_NAME_CHARS = 120

export function isAlertableNodeMetric(
  metric: string
): metric is AlertableNodeMetric {
  return (ALERTABLE_NODE_METRICS as readonly string[]).includes(metric)
}

/** Metric label, falling back to the raw metric name for rules we do not edit. */
export function nodeAlertMetricLabel(metric: string): string | undefined {
  return isAlertableNodeMetric(metric)
    ? NODE_ALERT_METRIC_LABELS[metric]
    : undefined
}

export const nodeAlertRuleFormSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Give the rule a name')
    .max(MAX_RULE_NAME_CHARS, `Use at most ${MAX_RULE_NAME_CHARS} characters`),
  metric_name: z.enum(ALERTABLE_NODE_METRICS),
  comparator: z.enum(NODE_ALERT_COMPARATORS),
  threshold: z
    .number({ error: 'Enter a number' })
    .min(0, 'Must be between 0 and 100')
    .max(100, 'Must be between 0 and 100'),
  severity: z.enum(NODE_ALERT_SEVERITIES),
  for_minutes: z
    .number({ error: 'Enter a number of minutes' })
    .int('Use whole minutes')
    .min(0, 'Cannot be negative')
    .max(MAX_FOR_DURATION_SECS / 60, 'At most 1440 minutes (one day)'),
})

export type NodeAlertRuleFormValues = z.infer<typeof nodeAlertRuleFormSchema>

/** Default rule name for a metric, e.g. "High CPU usage". */
export function defaultRuleName(metric: AlertableNodeMetric): string {
  return `High ${NODE_ALERT_METRIC_LABELS[metric].toLowerCase()}`
}

export type RecommendedNodeAlert = {
  name: string
  metric_name: AlertableNodeMetric
  comparator: '>'
  threshold: number
  severity: 'warning' | 'critical'
  for_duration_secs: number
}

/**
 * A starting point, offered explicitly and never applied on its own. One
 * rule per metric because the server allows one: disk therefore gets the
 * warning, and the critical level is the operator's to add by editing.
 */
export const RECOMMENDED_NODE_ALERTS: readonly RecommendedNodeAlert[] = [
  {
    name: 'High CPU usage',
    metric_name: 'node.cpu_percent',
    comparator: '>',
    threshold: 90,
    severity: 'warning',
    for_duration_secs: 300,
  },
  {
    name: 'High memory usage',
    metric_name: 'node.memory_percent',
    comparator: '>',
    threshold: 90,
    severity: 'warning',
    for_duration_secs: 300,
  },
  {
    name: 'Disk nearly full',
    metric_name: 'node.disk_percent',
    comparator: '>',
    threshold: 85,
    severity: 'warning',
    for_duration_secs: 300,
  },
]

/** Recommendations for metrics that have no rule yet; never duplicates one. */
export function missingRecommendedAlerts(
  existing: ReadonlyArray<{ metric_name: string }>
): RecommendedNodeAlert[] {
  const covered = new Set(existing.map((rule) => rule.metric_name))
  return RECOMMENDED_NODE_ALERTS.filter((r) => !covered.has(r.metric_name))
}

/** Metrics that can still get a rule (the server allows one per metric). */
export function metricsWithoutRule(
  existing: ReadonlyArray<{ metric_name: string }>
): AlertableNodeMetric[] {
  const covered = new Set(existing.map((rule) => rule.metric_name))
  return ALERTABLE_NODE_METRICS.filter((m) => !covered.has(m))
}

/** Rules on the control plane are re-seeded on restart, so only workers' can be deleted. */
export function canDeleteNodeRules(nodeId: number): boolean {
  return nodeId !== 0
}

export function toCreateRequest(
  values: NodeAlertRuleFormValues
): NodeCreateAlertRuleRequest {
  return {
    name: values.name.trim(),
    metric_name: values.metric_name,
    comparator: values.comparator,
    threshold: values.threshold,
    severity: values.severity,
    for_duration_secs: values.for_minutes * 60,
    enabled: true,
  }
}
