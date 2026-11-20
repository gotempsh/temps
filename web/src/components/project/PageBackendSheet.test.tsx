// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { PageBackendResponse } from '@/api/client'
import { PageBackendContent } from './PageBackendSheet'

const base: PageBackendResponse = {
  path: '/pricing',
  start_time: '2026-11-19T12:00:00Z',
  end_time: '2026-11-20T12:00:00Z',
  vitals: {
    page_loads: 30,
    ttfb_p75_ms: 400,
    fcp_p75_ms: 900,
    lcp_p75_ms: 1800,
    inp_p75_ms: null,
  },
  configured: false,
  reason:
    'No backend request of this project for /pricing was traced in this window.',
  setup_path: 'traces',
  requests: 0,
  errors: 0,
  p50_ms: null,
  p75_ms: null,
  p95_ms: null,
  backend_share_of_ttfb: null,
  services: [],
  slowest: [],
}

const render = (data: PageBackendResponse) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <PageBackendContent data={data} projectSlug="shop" />
    </MemoryRouter>
  )

describe('PageBackendContent', () => {
  test('explains a page with no traced backend and links to traces', () => {
    const html = render(base)
    expect(html).toContain('No backend request of this project for /pricing')
    expect(html).toContain('href="/projects/shop/traces"')
    expect(html).toContain('400ms')
  })

  test('says how much of the wait the backend explains and links each slow request', () => {
    const html = render({
      ...base,
      configured: true,
      reason: null,
      setup_path: null,
      requests: 12,
      errors: 1,
      p50_ms: 120,
      p75_ms: 300,
      p95_ms: 900,
      backend_share_of_ttfb: 0.75,
      services: [{ service_name: 'storefront', requests: 12 }],
      slowest: [
        {
          trace_id: '0af7651916cd43dd8448eb211c80319c',
          span_name: 'GET /pricing',
          service_name: 'storefront',
          start_time: '2026-11-20T10:00:00Z',
          duration_ms: 900,
          error: true,
        },
      ],
    })
    expect(html).toContain('12 backend requests for this path, 1 failed')
    expect(html).toContain('about 75% of the time to first byte')
    expect(html).toContain(
      '/projects/shop/traces/0af7651916cd43dd8448eb211c80319c'
    )
    expect(html).toContain('error')
  })
})
