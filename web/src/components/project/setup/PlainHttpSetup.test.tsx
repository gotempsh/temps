// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render as mount } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { ingestSetupFixture, signalFixture } from '@/lib/ingest-setup.fixture'
import { installBrowserWindow } from '@/test/dom'
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

installBrowserWindow()
afterEach(cleanup)

const project = { id: 42, name: 'My app', slug: 'my-app' }

function render(
  signal: Parameters<typeof signalFixture>[0],
  configured = true,
  defaultRawOpen = false
) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <PlainHttpSetupView
        project={project}
        setup={signalFixture(signal, { configured })}
        origin="https://console.example.com"
        nowMs={1_795_000_000_000}
        defaultRawOpen={defaultRawOpen}
      />
    </MemoryRouter>
  )
}

const ALL_SIGNALS = [
  'analytics',
  'session_replay',
  'errors',
  'logs',
  'traces',
] as const

test('every signal leads with a library and folds curl away', () => {
  for (const signal of ALL_SIGNALS) {
    const html = render(signal)
    expect(html).toContain('1. Get the credential')
    expect(html).toContain('Without a library: endpoints and a curl test')
    expect(html).not.toContain('curl -i -X POST')
    expect(html).toContain('Hand off to agent')
  }
})

test('setup code blocks have no title bar, like the docs', () => {
  for (const signal of ALL_SIGNALS) {
    const html = render(signal, true, true)
    // Untitled blocks float their controls; titled ones render a header bar.
    expect(html).toContain('group-hover/code:opacity-100')
    expect(html).not.toContain('h-9 items-center justify-between')
  }
})

test('the raw HTTP details carry endpoints, headers and the curl test', () => {
  for (const signal of ALL_SIGNALS) {
    const html = render(signal, true, true)
    expect(html).toContain('Test with curl')
    expect(html).toContain('curl -i -X POST')
    expect(html).toContain('POST')
  }
  const analytics = render('analytics', true, true)
  expect(analytics).toContain('https://temps.example.com/api/_temps/event')
  expect(analytics).toContain('X-Temps-Analytics-Key: $TEMPS_ANALYTICS_KEY')
  expect(analytics).toContain('&quot;custom&quot;:')
  expect(render('traces', true, true)).toContain('Bearer $TEMPS_API_KEY')
})

test('a page that already shows the key can drop the credential step', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <PlainHttpSetupView
        project={project}
        setup={signalFixture('analytics')}
        origin="https://console.example.com"
        showCredentialStep={false}
      />
    </MemoryRouter>
  )
  expect(html).not.toContain('Get the credential')
  expect(html).not.toContain('temps analytics keys create')
  expect(html).toContain('Track from any browser code')
  expect(html).not.toContain('2. Track from any browser code')
})

test('the DSN the page already loaded is shown in step 1', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <PlainHttpSetupView
        project={project}
        setup={signalFixture('errors')}
        origin="https://console.example.com"
        credentialValue="https://abc@temps.example.com/42"
      />
    </MemoryRouter>
  )
  expect(html).toContain('SENTRY_DSN=https://abc@temps.example.com/42')
  expect(html).toContain('or use this one:')
})

test('analytics offers browser code without the SDK', () => {
  const html = render('analytics')
  expect(html).toContain('TEMPS_URL')
  expect(html).toContain('sendBeacon')
  expect(html).not.toContain('Create credential')
})

test('errors lead with the Sentry SDK reading the DSN from the environment', () => {
  const html = render('errors')
  expect(html).toContain('2. Install a Sentry SDK')
  expect(html).toContain('any official Sentry SDK works')
  expect(html).toContain('@sentry/node')
  expect(html).toContain('process.env.SENTRY_DSN')
  expect(html).toContain('href="https://docs.sentry.io/platforms/"')
})

test('errors without a DSN onboard with what is missing and a deep link', () => {
  const html = render('errors', false)
  expect(html).toContain('No active DSN exists for this project yet')
  expect(html).toContain('href="/projects/my-app/errors/setup"')
})

test('library tabs wrap to a second row instead of scrolling sideways', () => {
  const view = mount(
    <MemoryRouter>
      <PlainHttpSetupView
        project={project}
        setup={signalFixture('errors')}
        origin="https://console.example.com"
      />
    </MemoryRouter>
  )
  const strip = view.getByRole('tablist', { name: 'Sentry SDK examples' })
  expect(strip.className).toContain('flex-wrap')
  expect(strip.className).not.toContain('overflow-x-auto')
  expect(view.getAllByRole('tab')).toHaveLength(5)
})

test('session replay leads with the Temps SDK and offers rrweb', () => {
  const html = render('session_replay')
  expect(html).toContain('enableSessionRecording')
  expect(html).toContain('https://temps.example.com/api/_temps')
  expect(html).toContain('Any framework (rrweb)')
})

test('logs and traces lead with OpenTelemetry and keep the key server-side', () => {
  for (const signal of ['logs', 'traces'] as const) {
    const html = render(signal)
    expect(html).toContain('2. Export with OpenTelemetry')
    expect(html).toContain(`https://temps.example.com/api/otel/v1/${signal}`)
    expect(html).toContain('OTel Collector')
    expect(html).toContain('never in browser code')
    expect(html).toContain('href="/settings/keys/new?name=my-app-telemetry"')
  }
})

test('opening the raw HTTP section shows the curl test', () => {
  const view = mount(
    <MemoryRouter>
      <PlainHttpSetupView
        project={project}
        setup={signalFixture('errors')}
        origin="https://console.example.com"
      />
    </MemoryRouter>
  )
  expect(view.container.textContent).not.toContain('curl -i -X POST')
  fireEvent.click(
    view.getByRole('button', {
      name: /Without a library: endpoints and a curl test/,
    })
  )
  expect(view.getByText('Test with curl')).toBeTruthy()
  expect(view.container.textContent).toContain('curl -i -X POST')
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
  for (const signal of ALL_SIGNALS) {
    const html = render(signal, true, true)
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
  expect(html).toContain('Hand off to agent')
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
  expect(loaded).toContain('Hand off to agent')
  expect(loaded).not.toContain('Retry prompt')
})
