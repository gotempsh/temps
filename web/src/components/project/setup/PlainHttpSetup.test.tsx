// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { signalFixture } from '@/lib/ingest-setup.fixture'
import { PlainHttpSetupView } from './PlainHttpSetup'

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
    expect(html).toContain('href="/keys/new"')
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
