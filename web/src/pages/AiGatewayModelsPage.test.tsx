// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { AiGatewayModelsPage } from './AiGatewayModelsPage'

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
        <AiGatewayModelsPage />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const settings = {
  source_url: null,
  effective_source_url: 'https://catalog.example.com/model-catalog.json',
  default_source_url: 'https://catalog.example.com/model-catalog.json',
  auto_refresh: true,
  refresh_interval_hours: 24,
  discover_provider_models: true,
}

function status(overrides: Record<string, unknown> = {}) {
  return {
    settings,
    last_attempt_at: null,
    last_refreshed_at: null,
    last_source_url: null,
    last_error: null,
    entry_count: 0,
    skipped_count: 0,
    applied_price_count: 0,
    bundled_model_count: 120,
    last_discovery_at: null,
    next_refresh_at: null,
    refreshing: false,
    ...overrides,
  }
}

const agents = {
  settings: { auto_update: false, updated_at: null },
  agents: [
    {
      provider_id: 'claude_cli',
      name: 'Claude Code',
      binary: 'claude',
      npm_package: '@anthropic-ai/claude-code',
      latest_version: '2.1.330',
      checked_at: '2026-11-23T10:00:00Z',
      error: null,
    },
  ],
}

test('loading shows skeletons, not spinners', () => {
  const markup = render(() => {})
  expect(markup).toContain('Loading the model catalog')
  expect(markup).toContain('animate-pulse')
  expect(markup).not.toContain('animate-spin')
})

test('a fresh instance onboards: default source, never refreshed, Refresh now', () => {
  const markup = render((client) => {
    client.setQueryData(['aiModelCatalog', 'status'], status())
    client.setQueryData(['aiModelCatalog', 'models', '__all', '', 1], {
      items: [],
      total: 0,
      page: 1,
      page_size: 20,
    })
    client.setQueryData(['aiCodingAgents', 'overview'], agents)
  })
  expect(markup).toContain('Temps catalog')
  expect(markup).toContain('never')
  expect(markup).toContain('Refresh now')
  expect(markup).toContain('No catalog stored yet')
  expect(markup).toContain('Claude Code')
  expect(markup).toContain('2.1.330')
  expect(markup).toContain('Update workspaces in use automatically')
})

test('a failed refresh, unpriced models and a workspace behind are all shown', () => {
  const markup = render((client) => {
    client.setQueryData(
      ['aiModelCatalog', 'status'],
      status({
        last_error: 'the catalog source answered HTTP 503',
        entry_count: 800,
        applied_price_count: 640,
        last_refreshed_at: '2026-11-22T10:00:00Z',
      })
    )
    client.setQueryData(['aiModelCatalog', 'unpriced'], {
      models: [
        {
          provider_key_id: 3,
          provider_key_name: 'Team key',
          provider: 'openai',
          model_id: 'vendor-model-next',
        },
      ],
      total: 1,
    })
    client.setQueryData(['aiCodingAgents', 'overview'], agents)
    client.setQueryData(['aiCodingAgents', 'workspace', 'claude_cli'], {
      provider_id: 'claude_cli',
      installed_version: '2.1.263',
      latest_version: '2.1.330',
      update_available: true,
      updating: false,
      checked_at: null,
      detail: null,
    })
  })
  expect(markup).toContain('The last refresh failed')
  expect(markup).toContain('the catalog source answered HTTP 503')
  expect(markup).toContain('1 enabled model has no known price')
  expect(markup).toContain('vendor-model-next')
  expect(markup).toContain('2.1.263')
  expect(markup).toContain('2.1.330 available')
})
