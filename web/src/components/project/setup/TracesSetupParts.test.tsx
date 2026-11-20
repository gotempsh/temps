// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import type { EnvironmentResponse } from '@/api/client'
import { ingestSetupFixture } from '@/lib/ingest-setup.fixture'
import {
  FAILURE_DETAIL,
  ingestSetupClient,
  type IngestSetupCacheState,
} from './ingest-setup-cache.test-helpers'
import {
  ExternalOtelEnvBlock,
  SetupEnvironmentSelect,
} from './TracesSetupParts'

function renderEnvBlock(state: IngestSetupCacheState) {
  return renderToStaticMarkup(
    <QueryClientProvider client={ingestSetupClient(state)}>
      <ExternalOtelEnvBlock projectId={42} serviceName="my-app" />
    </QueryClientProvider>
  )
}

test('the environment select is named by its visible label and value', () => {
  const html = renderToStaticMarkup(
    <SetupEnvironmentSelect
      id="traces-env"
      environments={[{ id: 7, name: 'production' } as EnvironmentResponse]}
      value="7"
      onValueChange={() => {}}
    />
  )
  expect(html).toContain('id="traces-env-label"')
  expect(html).toContain('id="traces-env"')
  expect(html).toContain('aria-labelledby="traces-env-label traces-env"')
  expect(html).toContain('Environment:')
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
})
