// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { getHealthcheckDefaultsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  HealthcheckDefaults,
  HealthcheckTimings,
} from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { ServiceHealthchecksPage } from './ServiceHealthchecksPage'

const REDIS: HealthcheckTimings = {
  interval_secs: 30,
  start_interval_secs: 2,
  start_period_secs: 90,
  timeout_secs: 5,
  retries: 3,
}

const DEFAULTS: HealthcheckDefaults = {
  overrides: {
    interval_secs: 60,
    start_interval_secs: null,
    start_period_secs: null,
    timeout_secs: null,
    retries: null,
  },
  engines: [
    {
      engine: 'redis',
      builtin: REDIS,
      effective: {
        timings: { ...REDIS, interval_secs: 60 },
        sources: {
          interval_secs: 'instance_default',
          start_interval_secs: 'builtin',
          start_period_secs: 'builtin',
          timeout_secs: 'builtin',
          retries: 'builtin',
        },
      },
      readiness_wait_secs: 90,
    },
  ],
}

function renderAdmin(queryClient: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Example Admin',
              username: 'example',
              avatar_url: '',
              mfa_enabled: false,
              role: 'admin',
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <ServiceHealthchecksPage />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a failed background refresh keeps the loaded form and says so', () => {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  // Loaded once, then a refetch failed: TanStack keeps the data.
  queryClient
    .getQueryCache()
    .build(queryClient, { queryKey: getHealthcheckDefaultsQueryKey() })
    .setState({
      status: 'error',
      data: DEFAULTS,
      dataUpdatedAt: Date.now(),
      error: { title: 'Bad Gateway', status: 502, detail: 'upstream reset' },
      fetchStatus: 'idle',
    } as never)

  const html = renderAdmin(queryClient)
  expect(html).toContain('Could not refresh the health check defaults')
  expect(html).toContain('aria-label="Healthcheck overrides"')
  // The cached engine table still shows what each engine gets.
  expect(html).toContain('What each engine gets')
  expect(html).toContain('Instance default</div>')
  queryClient.clear()
})
