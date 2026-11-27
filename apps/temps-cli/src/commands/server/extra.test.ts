// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { alertRuleRequest, metricWindowQuery, nodeIdList } from './extra.js'
import { registerServerCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('server metric helpers', () => {
  test('metricWindowQuery validates times', () => {
    expect(metricWindowQuery({ start: '2026-01-01T00:00:00Z', end: '2026-01-02T00:00:00Z' })).toEqual({
      start_time: '2026-01-01T00:00:00Z',
      end_time: '2026-01-02T00:00:00Z',
    })
    expect(metricWindowQuery({})).toEqual({})
    expect(() => metricWindowQuery({ start: 'yesterday' })).toThrow(/--start/)
  })

  // The server refuses percentile on metric ranges (#2075): offering the flag
  // would turn every use of it into an error.
  test('server metrics offers no --percentile flag', async () => {
    const run = await runWithApi(registerServerCommands, ['server', 'metrics', '--metric', 'node.cpu_percent', '--percentile', '0.9'], {
      'GET /nodes/0/metrics': [],
    })
    expect(run.error?.message).toMatch(/unknown option '--percentile'/)
    expect(run.requests).toEqual([])
  })

  test('nodeIdList and alertRuleRequest', () => {
    expect(nodeIdList('0, 1,2')).toBe('0,1,2')
    expect(() => nodeIdList('a')).toThrow(/--nodes/)
    expect(alertRuleRequest({ name: 'cpu', metric: 'node.cpu_percent', comparator: 'gt', threshold: '90', severity: 'critical', for: '60' })).toEqual({
      name: 'cpu',
      metric_name: 'node.cpu_percent',
      comparator: '>',
      threshold: 90,
      severity: 'critical',
      for_duration_secs: 60,
    })
    expect(() => alertRuleRequest({ name: 'x', metric: 'm', comparator: '=', threshold: '1', severity: 'warning' })).toThrow(/--comparator/)
    expect(() => alertRuleRequest({ name: 'x', metric: 'm', comparator: '>', threshold: '1', severity: 'info' })).toThrow(/--severity/)
  })
})

describe('temps server (extras)', () => {
  const routes = {
    'GET /nodes/1/metrics': [],
    'GET /nodes/metrics/cluster': [],
    'GET /nodes/metrics/cluster/latest': {},
    'GET /nodes/metrics/reporting': [],
    'GET /nodes/2/metrics/disk-health': { arrays: [] },
    'POST /nodes/2/metrics/alert-rules': { id: 8 },
    'DELETE /nodes/2/metrics/alert-rules/8': new Response(null, { status: 204 }),
  }

  test.each([
    [['metrics', '--metric', 'node.cpu_percent', '--node', '1', '--start', '2026-01-01T00:00:00Z'], 'GET /nodes/1/metrics?metric=node.cpu_percent&range=1h&start_time=2026-01-01T00%3A00%3A00Z'],
    [['cluster-metrics', '--metric', 'node.cpu_percent', '--nodes', '0,1', '--range', '24h'], 'GET /nodes/metrics/cluster?metric=node.cpu_percent&node_ids=0%2C1&range=24h'],
    [['cluster-latest', '--nodes', '0,1'], 'GET /nodes/metrics/cluster/latest?node_ids=0%2C1'],
    [['reporting'], 'GET /nodes/metrics/reporting'],
    [['disk-health', '--node', '2'], 'GET /nodes/2/metrics/disk-health'],
    [['alert-rules', 'create', '--node', '2', '--name', 'disk', '--metric', 'node.disk_used_percent', '--comparator', '>=', '--threshold', '90'], 'POST /nodes/2/metrics/alert-rules'],
    [['alert-rules', 'delete', '8', '--node', '2', '--yes'], 'DELETE /nodes/2/metrics/alert-rules/8'],
  ])('server %p', async (argv, request) => {
    const run = await runWithApi(registerServerCommands, ['server', ...argv, ...(argv[0] === 'alert-rules' && argv[1] === 'delete' ? [] : ['--json'])], routes)
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([request])
  })
})
