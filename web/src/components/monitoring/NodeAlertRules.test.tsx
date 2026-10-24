// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { nodeMetricsGetAlertRulesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ServiceAlertRuleResponse } from '@/api/client/types.gen'
import { NodeAlertRules } from './NodeAlertRules'

function rule(
  id: number,
  metric_name: string,
  name: string
): ServiceAlertRuleResponse {
  return {
    id,
    name,
    metric_name,
    comparator: '>',
    threshold: 90,
    severity: 'warning',
    for_duration_secs: 300,
    enabled: true,
  }
}

function render(nodeId: number | undefined, rules: ServiceAlertRuleResponse[]) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  queryClient.setQueryData(
    nodeMetricsGetAlertRulesOptions({ path: { id: nodeId ?? 0 } }).queryKey,
    rules
  )
  const html = renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <NodeAlertRules nodeId={nodeId} />
    </QueryClientProvider>
  )
  queryClient.clear()
  return html
}

describe('NodeAlertRules', () => {
  test('a worker can delete its rules', () => {
    const html = render(5, [rule(1, 'node.cpu_percent', 'High CPU usage')])
    expect(html).toContain('Delete High CPU usage')
    expect(html).toContain('CPU usage')
  })

  test('the control plane offers no delete, because its defaults are re-seeded', () => {
    const html = render(undefined, [
      rule(1, 'node.fd_percent', 'System FD usage'),
    ])
    expect(html).not.toContain('Delete System FD usage')
    expect(html).toContain('only disabling sticks')
    expect(html).toContain('Platform alert rules')
  })

  test('a worker with no rules is told what to do, with the recommended levels', () => {
    const html = render(5, [])
    expect(html).toContain('No alert rules for this worker yet')
    expect(html).toContain('Add recommended alerts')
    expect(html).toContain('Add alert rule')
  })

  test('recommended alerts are not offered once every metric has a rule', () => {
    const html = render(5, [
      rule(1, 'node.cpu_percent', 'High CPU usage'),
      rule(2, 'node.memory_percent', 'High memory usage'),
      rule(3, 'node.disk_percent', 'Disk nearly full'),
    ])
    expect(html).not.toContain('Add recommended alerts')
  })

  test('only the metrics still missing a rule are recommended', () => {
    const html = render(5, [rule(1, 'node.cpu_percent', 'High CPU usage')])
    expect(html).toContain('Add recommended alerts')
    expect(html).toContain('Memory usage &gt; 90%')
    expect(html).not.toContain('CPU usage &gt; 90%')
  })
})
