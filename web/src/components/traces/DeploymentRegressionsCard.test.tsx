// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import {
  getDeploymentRegressionsOptions,
  getProjectDeploymentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { DeploymentRegressionsCard } from './DeploymentRegressionsCard'

const PROJECT_ID = 3

function render(prepare: (queryClient: QueryClient) => void) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retryOnMount: false } },
  })
  prepare(queryClient)
  return renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={queryClient}>
        <DeploymentRegressionsCard
          projectId={PROJECT_ID}
          projectSlug="shop"
          onOpenTrend={() => {}}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

const deploymentsKey = getProjectDeploymentsOptions({
  path: { id: PROJECT_ID },
  query: { per_page: 20 },
}).queryKey

describe('DeploymentRegressionsCard', () => {
  test('explains what it would show and links to deployments when none went live', () => {
    const html = render((queryClient) => {
      queryClient.setQueryData(deploymentsKey, {
        deployments: [],
        total: 0,
        page: 1,
        per_page: 20,
      } as never)
    })
    expect(html).toContain('No deployment of this project has gone live yet')
    expect(html).toContain('40% slower')
    expect(html).toContain('href="/projects/shop/deployments"')
  })

  test('lists a regressed operation with its p95 before and after', () => {
    const html = render((queryClient) => {
      queryClient.setQueryData(deploymentsKey, {
        deployments: [
          { id: 812, status: 'deployed', created_at: 1_700_000_000 },
        ],
        total: 1,
        page: 1,
        per_page: 20,
      } as never)
      queryClient.setQueryData(
        getDeploymentRegressionsOptions({
          query: { project_id: PROJECT_ID, deployment_id: 812 },
        }).queryKey,
        {
          deployment: {
            deployment_id: 812,
            slug: 'deploy-812',
            environment_id: 1,
            live_at: '2026-11-19T12:20:00Z',
          },
          baseline_start: '2026-11-12T12:00:00Z',
          after_start: '2026-11-19T13:00:00Z',
          after_end: '2026-11-19T18:00:00Z',
          changes: [
            {
              service_name: 'checkout-api',
              span_name: 'GET /cart',
              change: 'regressed',
              baseline_p95_ms: 100,
              after_p95_ms: 140,
              change_ratio: 0.4,
              baseline_hours: 24,
              after_hours: 5,
            },
          ],
          regressed: 1,
          improved: 0,
          unchanged: 6,
          insufficient_baseline: 0,
          insufficient_after: 2,
          min_after_hours: 2,
          tracking: {
            configured: true,
            tracked_operations: 9,
            overflow_operations: 0,
            max_tracked_operations: 200,
            retention_days: 30,
          },
        }
      )
    })
    expect(html).toContain('1 slower, 0 faster')
    expect(html).toContain('2 still waiting for 2 complete hours')
    expect(html).toContain('GET /cart')
    expect(html).toContain('+40%')
    expect(html).toContain('p95 100ms → 140ms')
  })
})
