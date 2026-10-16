// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { monthStartIso, usdToMicrocents } from '@/lib/ai-gateway-limits'
import { AiGatewayLimitsPage } from './AiGatewayLimitsPage'

function render(seed: (client: QueryClient) => void) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  seed(client)
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AiGatewayLimitsPage />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function seedOverview(client: QueryClient, overrides: unknown[]) {
  client.setQueryData(['aiLimits', 'overview', 1], {
    instance: {
      max_requests_per_minute: null,
      max_cost_per_month_microcents: null,
    },
    built_in_max_requests_per_minute: 300,
    project_overrides: overrides,
    total_project_overrides: overrides.length,
    page: 1,
    page_size: 20,
  })
  client.setQueryData(['aiLimits', 'projects'], {
    projects: [{ id: 7, name: 'Storefront', slug: 'storefront' }],
  })
}

test('shows the built-in default and an onboarding empty state with no overrides', () => {
  const markup = render((client) => {
    seedOverview(client, [])
    client.setQueryData(['aiLimits', 'spend', monthStartIso(new Date())], [])
  })
  expect(markup).toContain('300 / min')
  expect(markup).toContain('Built-in default')
  expect(markup).toContain('No project overrides')
  expect(markup).toContain('No attributed usage yet')
  expect(markup).toContain('Add override')
  expect(markup).toContain('Edit defaults')
})

test('lists overrides with inherit, unlimited and capped values distinct', () => {
  const markup = render((client) => {
    seedOverview(client, [
      {
        project_id: 7,
        limits: {
          max_requests_per_minute: 0,
          max_cost_per_month_microcents: usdToMicrocents(25),
        },
      },
      {
        project_id: 9,
        limits: {
          max_requests_per_minute: 60,
          max_cost_per_month_microcents: null,
        },
      },
    ])
    client.setQueryData(
      ['aiLimits', 'spend', monthStartIso(new Date())],
      [
        {
          project_id: 7,
          request_count: 1200,
          input_tokens: 1000,
          output_tokens: 500,
          estimated_cost_microcents: usdToMicrocents(3.5),
          byok_request_count: 0,
          error_count: 2,
        },
      ]
    )
  })
  expect(markup).toContain('Storefront')
  expect(markup).toContain('Project #9')
  expect(markup).toContain('Unlimited')
  expect(markup).toContain('$25.00 / month')
  expect(markup).toContain('60 / min')
  expect(markup).toContain('Inherited')
  expect(markup).toContain('$3.50')
  expect(markup).toContain('Edit limits for Storefront')
  expect(markup).toContain('Reset limits for Project #9')
})

test('a forbidden read explains the access denial instead of showing an empty list', () => {
  const markup = render((client) => {
    const query = client.getQueryCache().build(client, {
      queryKey: ['aiLimits', 'overview', 1],
    })
    query.setState({
      ...query.state,
      status: 'error',
      error: { title: 'Forbidden', status: 403, detail: 'admin only' } as never,
      fetchStatus: 'idle',
    })
  })
  expect(markup).toContain('AI limits: access denied')
  expect(markup).not.toContain('No project overrides')
  expect(markup).not.toContain('Add override')
})
