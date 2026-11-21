// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { getProjectOptions } from '@/api/client/@tanstack/react-query.gen'
import { QueryContent } from './GlobalPage'
import { ObservabilitySetupGuide } from './ObservabilitySetupGuide'
import {
  serviceSetupHref,
  traceEnvironment,
  type ObservabilityKind,
} from './observability-setup'

function render(node: React.ReactNode, client = new QueryClient()) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>{node}</QueryClientProvider>
    </MemoryRouter>
  )
}

describe('empty observability pages teach the setup', () => {
  const expected: Record<ObservabilityKind, string[]> = {
    analytics: [
      'No analytics yet',
      '@temps-sdk/react-analytics',
      'ingestKey=&quot;&lt;ingest-key&gt;&quot;',
      'Visit a page',
    ],
    errors: [
      'No errors reported',
      '@sentry/node',
      'SENTRY_DSN automatically',
      'Send a test error',
    ],
    traces: [
      'No traces yet',
      '@opentelemetry/sdk-node',
      'OTEL_EXPORTER_OTLP_ENDPOINT=https://&lt;your-temps-domain&gt;/api/otel',
      'Make a request',
    ],
    logs: [
      'No logs yet',
      'collects the output of every container',
      'href="/projects/new"',
      'Write to standard output',
    ],
  }
  for (const [kind, needles] of Object.entries(expected)) {
    test(`${kind} lists numbered steps with code`, () => {
      const html = render(
        <ObservabilitySetupGuide kind={kind as ObservabilityKind} />
      )
      for (const needle of needles) expect(html).toContain(needle)
      expect(html).toContain('<ol')
      // Without a service in scope, the guide asks for one.
      expect(html).toContain('Choose a service')
    })
  }

  test('with a service in scope, the guide opens that service’s setup', () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    })
    client.setQueryData<object>(
      getProjectOptions({ path: { id: 7 } }).queryKey,
      {
        id: 7,
        name: 'Storefront',
        slug: 'storefront',
      }
    )
    const html = render(
      <ObservabilitySetupGuide kind="errors" projectId={7} />,
      client
    )
    expect(html).toContain('href="/projects/storefront/errors/setup"')
    expect(html).toContain('Open error tracking setup for Storefront')
  })

  test('manual trace export names the project, as API-key ingest requires', () => {
    // An API key identifies a user, so OTLP ingest also needs the project.
    expect(traceEnvironment()).toContain(
      'OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <api-key>,X-Temps-Project-Id=<project-id>'
    )
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    })
    client.setQueryData<object>(
      getProjectOptions({ path: { id: 7 } }).queryKey,
      { id: 7, name: 'Storefront', slug: 'storefront' }
    )
    const html = render(
      <ObservabilitySetupGuide kind="traces" projectId={7} />,
      client
    )
    expect(html).toContain('X-Temps-Project-Id=7')
    // The headers line is long: it wraps instead of scrolling sideways.
    expect(html).toContain('whitespace-pre-wrap [overflow-wrap:anywhere]')
  })

  test('each kind links to the service page that sets it up', () => {
    expect(serviceSetupHref('analytics', 'shop')).toBe(
      '/projects/shop/analytics/setup'
    )
    expect(serviceSetupHref('traces', 'shop')).toBe('/projects/shop/traces')
    // The project's own logs route is request logs, not container output.
    expect(serviceSetupHref('logs', 'shop')).toBe('/projects/shop/deployments')
    expect(serviceSetupHref('errors')).toBe('/projects')
  })
})

describe('QueryContent', () => {
  const base = {
    loading: false,
    error: null,
    title: 'Traces',
    retry: () => {},
    children: <p>rows</p>,
  }
  test('an empty, unfiltered view shows the setup guide', () => {
    const html = render(
      <QueryContent {...base} empty setup={<p>setup guide</p>} />
    )
    expect(html).toContain('setup guide')
    expect(html).not.toContain('No traces in this view')
  })
  test('an empty, filtered view says to change the filters', () => {
    const html = render(<QueryContent {...base} empty />)
    expect(html).toContain('No traces in this view')
  })
  test('rows are shown whatever the setup', () => {
    const html = render(
      <QueryContent {...base} empty={false} setup={<p>setup guide</p>} />
    )
    expect(html).toContain('rows')
    expect(html).not.toContain('setup guide')
  })
})
