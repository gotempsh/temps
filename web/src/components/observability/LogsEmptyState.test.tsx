// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { LogsEmptyState } from './LogsEmptyState'

test('offers telemetry setup from the first-use logs state', () => {
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <LogsEmptyState
          project={{ id: 42, name: 'Example', slug: 'example-project' }}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )

  expect(markup).toContain('No telemetry logs yet')
  expect(markup).toContain(
    'href="/projects/example-project/traces#traces-setup"'
  )
  expect(markup).toContain('Set up OpenTelemetry')
  expect(markup).toContain('href="/projects/example-project/errors/setup"')
  expect(markup).toContain('Set up Sentry')
  expect(markup).toContain('OpenTelemetry or a Sentry SDK')
  // The framework-agnostic option and its prompt sit beside the SDK choices.
  expect(markup).toContain('Any language or framework')
  expect(markup).toContain('Copy AI prompt')
})
