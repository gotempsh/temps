// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { ingestSetupFixture } from '@/lib/ingest-setup.fixture'
import {
  FAILURE_DETAIL,
  ingestSetupClient,
  type IngestSetupCacheState,
} from './ingest-setup-cache.test-helpers'
import { ExternalOtelEnvBlock, ProjectWideOtlpNote } from './TracesSetupParts'

function renderEnvBlock(state: IngestSetupCacheState) {
  return renderToStaticMarkup(
    <QueryClientProvider client={ingestSetupClient(state)}>
      <ExternalOtelEnvBlock projectId={42} serviceName="my-app" />
    </QueryClientProvider>
  )
}

test('the OTLP setup says the data belongs to the whole project', () => {
  const html = renderToStaticMarkup(<ProjectWideOtlpNote />)
  expect(html).toContain('belong to the whole project')
  expect(html).toContain('All environments')
})

test('the external .env shows the server-resolved OTLP variables once loaded', () => {
  const html = renderEnvBlock({ kind: 'loaded', data: ingestSetupFixture() })
  expect(html).toContain('OTEL_EXPORTER_OTLP_TRACES_ENDPOINT')
  expect(html).toContain('https://temps.example.com/api/otel/v1/traces')
})

test('a failed load says what failed and offers Retry instead of loading forever', () => {
  const html = renderEnvBlock({ kind: 'failed' })
  expect(html).toContain('Could not load the OTLP endpoint')
  expect(html).toContain(FAILURE_DETAIL)
  expect(html).toContain('Retry')
  expect(html).not.toContain('Loading')
  expect(html).not.toContain('OTEL_EXPORTER_OTLP_TRACES_ENDPOINT')
})

test('a cached setup stays usable after a failed refresh', () => {
  const html = renderEnvBlock({
    kind: 'failed-after-load',
    data: ingestSetupFixture(),
  })
  expect(html).toContain('OTEL_EXPORTER_OTLP_TRACES_ENDPOINT')
  expect(html).not.toContain('Could not load the OTLP endpoint')
  // ...and says the refresh failed, with a way to try again.
  expect(html).toContain('Could not refresh the OTLP endpoint')
  expect(html).toContain('Retry')
})

test('the .env block wraps instead of scrolling sideways', () => {
  const html = renderEnvBlock({ kind: 'loaded', data: ingestSetupFixture() })
  expect(html).toContain('whitespace-pre-wrap')
  expect(html).not.toMatch(/whitespace-pre(?!-wrap)/)
  expect(html).not.toContain('Could not refresh the OTLP endpoint')
})
