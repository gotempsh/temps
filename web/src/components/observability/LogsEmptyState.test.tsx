// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ingestSetupFixture } from '@/lib/ingest-setup.fixture'
import { installBrowserWindow } from '@/test/dom'
import { ingestSetupClient } from '@/components/project/setup/ingest-setup-cache.test-helpers'
import { LogsEmptyState } from './LogsEmptyState'

installBrowserWindow()
afterEach(cleanup)

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
  expect(markup).toContain('Hand off to agent')
})

test('closing the any-framework setup keeps its state for reopening', () => {
  const view = render(
    <QueryClientProvider
      client={ingestSetupClient({ kind: 'loaded', data: ingestSetupFixture() })}
    >
      <MemoryRouter>
        <LogsEmptyState project={{ id: 42, name: 'My app', slug: 'my-app' }} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const toggle = view.getByRole('button', { name: /Any language or framework/ })
  const panel = view.container.querySelector('#logs-any-framework-setup')!
  expect(toggle.getAttribute('aria-controls')).toBe('logs-any-framework-setup')
  expect(panel.getAttribute('data-state')).toBe('closed')

  fireEvent.click(toggle)
  expect(panel.getAttribute('data-state')).toBe('open')
  fireEvent.click(
    view.getByRole('button', {
      name: /Without a library: endpoints and a curl test/,
    })
  )
  expect(view.getByText('Test with curl')).toBeTruthy()

  fireEvent.click(toggle) // close
  expect(panel.getAttribute('data-state')).toBe('closed')
  // Still mounted: reopening shows the raw details the user expanded.
  expect(view.container.querySelector('#logs-any-framework-setup')).toBe(panel)
  fireEvent.click(toggle)
  expect(view.getByText('Test with curl')).toBeTruthy()
})
