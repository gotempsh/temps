// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ALERTABLE_NODE_METRICS,
  RECOMMENDED_NODE_ALERTS,
  canDeleteNodeRules,
  defaultRuleName,
  isAlertableNodeMetric,
  metricsWithoutRule,
  missingRecommendedAlerts,
  nodeAlertRuleFormSchema,
  toCreateRequest,
} from './node-alert-rules'

const valid = {
  name: 'High CPU usage',
  metric_name: 'node.cpu_percent',
  comparator: '>',
  threshold: 90,
  severity: 'warning',
  for_minutes: 5,
}

describe('nodeAlertRuleFormSchema', () => {
  test('accepts a complete rule', () => {
    expect(nodeAlertRuleFormSchema.safeParse(valid).success).toBe(true)
  })

  test('accepts the 0 and 100 bounds', () => {
    for (const threshold of [0, 100]) {
      expect(
        nodeAlertRuleFormSchema.safeParse({ ...valid, threshold }).success
      ).toBe(true)
    }
  })

  test('rejects thresholds outside 0-100', () => {
    for (const threshold of [-0.1, 100.1, 250]) {
      expect(
        nodeAlertRuleFormSchema.safeParse({ ...valid, threshold }).success
      ).toBe(false)
    }
  })

  test('rejects metrics the server cannot evaluate for a node', () => {
    for (const metric_name of [
      'node.fd_percent',
      'proxy.error_rate_percent',
      '',
    ]) {
      expect(
        nodeAlertRuleFormSchema.safeParse({ ...valid, metric_name }).success
      ).toBe(false)
    }
  })

  test('rejects blank names and unknown comparators or severities', () => {
    expect(
      nodeAlertRuleFormSchema.safeParse({ ...valid, name: '   ' }).success
    ).toBe(false)
    expect(
      nodeAlertRuleFormSchema.safeParse({ ...valid, comparator: '==' }).success
    ).toBe(false)
    expect(
      nodeAlertRuleFormSchema.safeParse({ ...valid, severity: 'info' }).success
    ).toBe(false)
  })

  test('limits the breach duration to whole minutes within a day', () => {
    expect(
      nodeAlertRuleFormSchema.safeParse({ ...valid, for_minutes: 0 }).success
    ).toBe(true)
    expect(
      nodeAlertRuleFormSchema.safeParse({ ...valid, for_minutes: 1440 }).success
    ).toBe(true)
    for (const for_minutes of [-1, 1441, 1.5]) {
      expect(
        nodeAlertRuleFormSchema.safeParse({ ...valid, for_minutes }).success
      ).toBe(false)
    }
  })
})

describe('recommended alerts', () => {
  test('every recommendation is itself a valid rule', () => {
    for (const rec of RECOMMENDED_NODE_ALERTS) {
      expect(isAlertableNodeMetric(rec.metric_name)).toBe(true)
      expect(
        nodeAlertRuleFormSchema.safeParse({
          name: rec.name,
          metric_name: rec.metric_name,
          comparator: rec.comparator,
          threshold: rec.threshold,
          severity: rec.severity,
          for_minutes: rec.for_duration_secs / 60,
        }).success
      ).toBe(true)
    }
  })

  test('offers one rule per metric, since the server allows only one', () => {
    const metrics = RECOMMENDED_NODE_ALERTS.map((r) => r.metric_name)
    expect(new Set(metrics).size).toBe(metrics.length)
    expect([...metrics].sort()).toEqual([...ALERTABLE_NODE_METRICS].sort())
  })

  test('offers everything when the node has no rules', () => {
    expect(missingRecommendedAlerts([])).toEqual([...RECOMMENDED_NODE_ALERTS])
  })

  test('skips metrics that already have a rule, enabled or not', () => {
    const missing = missingRecommendedAlerts([
      { metric_name: 'node.cpu_percent' },
      { metric_name: 'node.fd_percent' },
    ])
    expect(missing.map((r) => r.metric_name)).toEqual([
      'node.memory_percent',
      'node.disk_percent',
    ])
  })

  test('offers nothing once all three metrics are covered', () => {
    expect(
      missingRecommendedAlerts(
        ALERTABLE_NODE_METRICS.map((metric_name) => ({ metric_name }))
      )
    ).toEqual([])
  })
})

describe('helpers', () => {
  test('metricsWithoutRule lists only uncovered alertable metrics', () => {
    expect(
      metricsWithoutRule([
        { metric_name: 'node.memory_percent' },
        { metric_name: 'node.fd_percent' },
      ])
    ).toEqual(['node.cpu_percent', 'node.disk_percent'])
  })

  test('default names read as sentences', () => {
    expect(defaultRuleName('node.disk_percent')).toBe('High disk usage')
  })

  test('only workers can have rules deleted; the control plane re-seeds its own', () => {
    expect(canDeleteNodeRules(0)).toBe(false)
    expect(canDeleteNodeRules(4)).toBe(true)
  })
})

describe('toCreateRequest', () => {
  test('converts minutes to seconds, trims the name and enables the rule', () => {
    const parsed = nodeAlertRuleFormSchema.parse({
      ...valid,
      name: '  High CPU usage ',
      for_minutes: 5,
    })
    expect(toCreateRequest(parsed)).toEqual({
      name: 'High CPU usage',
      metric_name: 'node.cpu_percent',
      comparator: '>',
      threshold: 90,
      severity: 'warning',
      for_duration_secs: 300,
      enabled: true,
    })
  })
})
