// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { ingestSetupFixture, signalFixture } from '@/lib/ingest-setup.fixture'
import {
  FAILURE_DETAIL,
  ingestSetupClient,
  type IngestSetupCacheState,
} from './ingest-setup-cache.test-helpers'
import {
  CopySetupPromptButton,
  PlainHttpSetup,
  PlainHttpSetupView,
} from './PlainHttpSetup'

const project = { id: 42, name: 'My app', slug: 'my-app' }

function render(
  signal: Parameters<typeof signalFixture>[0],
  configured = true
) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <PlainHttpSetupView
        project={project}
        setup={signalFixture(signal, { configured })}
        origin="https://console.example.com"
        nowMs={1_795_000_000_000}
      />
    </MemoryRouter>
  )
}

test('analytics shows endpoint, key header, payload, curl and browser snippet', () => {
  const html = render('analytics')
  expect(html).toContain('https://temps.example.com/api/_temps/event')
  expect(html).toContain('X-Temps-Analytics-Key')
  expect(html).toContain('page_view')
  expect(html).toContain('sendBeacon')
  expect(html).toContain('curl -i -X POST')
  expect(html).toContain('Copy AI prompt')
  expect(html).not.toContain('Create credential')
})

test('errors without a DSN onboard with what is missing and a deep link', () => {
  const html = render('errors', false)
  expect(html).toContain('No active DSN exists for this project yet')
  expect(html).toContain('href="/projects/my-app/errors/setup"')
  expect(html).toContain('Any Sentry SDK works')
})

test('logs and traces show OTLP endpoint, secret-key warning and Collector config', () => {
  for (const signal of ['logs', 'traces'] as const) {
    const html = render(signal)
    expect(html).toContain(`https://temps.example.com/api/otel/v1/${signal}`)
    expect(html).toContain('Bearer $TEMPS_API_KEY')
    expect(html).toContain('never in browser code')
    expect(html).toContain('otlphttp/temps')
    expect(html).toContain('href="/settings/keys/new?name=my-app-telemetry"')
  }
})

test('creating a credential invalidates every environment of the ingest setup', async () => {
  // ErrorTrackingSetup and the analytics key card invalidate with the
  // project-only key; the setup views may be cached per environment.
  const { QueryClient } = await import('@tanstack/react-query')
  const { getProjectIngestSetupQueryKey } =
    await import('@/api/client/@tanstack/react-query.gen')
  const client = new QueryClient()
  const scoped = getProjectIngestSetupQueryKey({
    path: { project_id: 42 },
    query: { environment_id: 7 },
  })
  const other = getProjectIngestSetupQueryKey({ path: { project_id: 43 } })
  client.setQueryData(scoped, { stale: false })
  client.setQueryData(other, { stale: false })
  await client.invalidateQueries({
    queryKey: getProjectIngestSetupQueryKey({ path: { project_id: 42 } }),
  })
  expect(client.getQueryState(scoped)?.isInvalidated).toBe(true)
  expect(client.getQueryState(other)?.isInvalidated).toBe(false)
})

function renderWithCache(state: IngestSetupCacheState, node: React.ReactNode) {
  return renderToStaticMarkup(
    <QueryClientProvider client={ingestSetupClient(state)}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>
  )
}

test('every code block in the view wraps instead of scrolling sideways', () => {
  for (const signal of ['analytics', 'errors', 'logs', 'traces'] as const) {
    const html = render(signal)
    expect(html).toContain('whitespace-pre-wrap')
    // `whitespace-pre` alone is the unwrapped, horizontally scrolling mode.
    expect(html).not.toMatch(/whitespace-pre(?!-wrap)/)
  }
})

test('a failed refresh keeps the loaded setup visible with a Retry', () => {
  const html = renderWithCache(
    { kind: 'failed-after-load', data: ingestSetupFixture() },
    <PlainHttpSetup project={project} signal="traces" />
  )
  expect(html).toContain('Could not refresh the ingest endpoints')
  expect(html).toContain('Retry')
  expect(html).toContain('https://temps.example.com/api/otel/v1/traces')
  expect(html).toContain('Copy AI prompt')
  expect(html).not.toContain('Could not load the ingest endpoints')
})

test('a failed first load blocks the view with what failed and a Retry', () => {
  const html = renderWithCache(
    { kind: 'failed' },
    <PlainHttpSetup project={project} signal="traces" />
  )
  expect(html).toContain('Could not load the ingest endpoints')
  expect(html).toContain(FAILURE_DETAIL)
  expect(html).toContain('Retry')
  expect(html).not.toContain('/api/otel/v1/traces')
})

test('the prompt button offers a retry when the setup failed to load', () => {
  const failed = renderWithCache(
    { kind: 'failed' },
    <CopySetupPromptButton project={project} signal="logs" />
  )
  expect(failed).toContain('Retry prompt')
  expect(failed).not.toContain('disabled=""')

  const loaded = renderWithCache(
    { kind: 'loaded', data: ingestSetupFixture() },
    <CopySetupPromptButton project={project} signal="logs" />
  )
  expect(loaded).toContain('Copy AI prompt')
  expect(loaded).not.toContain('Retry prompt')
})
