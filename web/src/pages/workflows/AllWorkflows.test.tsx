// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AllWorkflows } from './AllWorkflows'
import { workflowTone } from './status'

function render(runs: object[], nextCursor: string | null = null) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(['all-workflow-runs', undefined], {
    pages: [{ runs, next_cursor: nextCursor }],
    pageParams: [undefined],
  })
  return renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <BreadcrumbProvider>
          <AllWorkflows />
        </BreadcrumbProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

describe('All workflows', () => {
  test('shows the empty state and a route to project setup', () => {
    const html = render([])
    expect(html).toContain('No workflow runs yet')
    expect(html).toContain('href="/projects"')
    expect(html).not.toContain('Load more runs')
  })
  test('links a run to its authorized project and environment and offers continuation', () => {
    const html = render(
      [
        {
          id: 'synthetic-run',
          project_id: 1,
          environment_id: 2,
          workflow: 'orders',
          status: 'waiting',
          created_at: '2026-01-01T00:00:00Z',
          step_count: 3,
        },
      ],
      'opaque'
    )
    expect(html).toContain('/workflows/1/2/runs/synthetic-run')
    expect(html).toContain('orders')
    expect(html).toContain('waiting')
    expect(html).toContain('Load more runs')
    expect(html).not.toContain('opaque')
  })
  test('uses failure and waiting states consistently', () => {
    expect(workflowTone('stuck')).toBe('error')
    expect(workflowTone('failed')).toBe('error')
    expect(workflowTone('waiting')).toBe('warn')
    expect(workflowTone('completed')).toBe('ok')
  })
})
