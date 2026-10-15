// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, render } from '@testing-library/react'
import axe from 'axe-core'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactElement } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { MonitoringSettings } from '@/components/monitoring/MonitoringSettings'
import { MONITORING_SECTIONS } from '@/components/monitoring/monitoring-sections'
import { AiOnboarding } from './AiOnboarding'
import CrossProjectTraceDetail from './CrossProjectTraceDetail'
import { Monitoring } from './Monitoring'
import ProxyLogs from './ProxyLogs'
import ProxyMetrics from './ProxyMetrics'
import Revenue from './Revenue'
import Sandboxes from './Sandboxes'
import WorkspaceDetail from './WorkspaceDetail'

/**
 * Page-level accessibility guard for the routes an axe sweep flagged with
 * critical findings (unnamed buttons, invalid ARIA values, prohibited ARIA
 * attributes, nested interactive controls).
 *
 * Each route is rendered with a mocked API, in its unconfigured/empty state
 * and, where it matters, with data, then checked against axe's WCAG A/AA rules.
 * Colour contrast and scroll-region focus depend on a layout engine, so they
 * are disabled here; scroll regions have their own source scan
 * (`components/ui/a11y-source-scan.test.ts`).
 */

installBrowserWindow()
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

const AXE_OPTIONS: axe.RunOptions = {
  runOnly: {
    type: 'tag',
    values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
  },
  rules: {
    // Need computed layout/colour, which happy-dom does not provide.
    'color-contrast': { enabled: false },
    'scrollable-region-focusable': { enabled: false },
    // Page-level rules: each route is rendered without the app shell.
    region: { enabled: false },
    bypass: { enabled: false },
    'document-title': { enabled: false },
    'html-has-lang': { enabled: false },
    'landmark-one-main': { enabled: false },
    'page-has-heading-one': { enabled: false },
  },
}

const TRACE = {
  trace_id: 'trace-1',
  start_time: '2026-10-12T10:00:00Z',
  end_time: '2026-10-12T10:00:01Z',
  total_duration_ms: 1000,
  span_count: 2,
  error_count: 0,
  has_redacted_spans: false,
  truncated: false,
  projects: [
    { project_id: 1, project_name: 'Example', project_slug: 'example' },
  ],
  spans: [
    {
      id: 'root',
      parent: null,
      name: 'GET /checkout',
      start: '10:00:00.000Z',
      ms: 1000,
    },
    {
      id: 'child',
      parent: 'root',
      name: 'db.query',
      start: '10:00:00.100Z',
      ms: 200,
    },
  ].map((s) => ({
    project_id: 1,
    project_name: 'Example',
    span: {
      attributes: {},
      duration_ms: s.ms,
      end_time: '2026-10-12T10:00:01Z',
      events: [],
      kind: 'SERVER',
      name: s.name,
      parent_span_id: s.parent,
      project_id: 1,
      resource: { attributes: {}, service_name: 'web' },
      span_id: s.id,
      start_time: `2026-10-12T${s.start}`,
      status_code: 'OK',
      status_message: '',
      trace_id: 'trace-1',
    },
  })),
}

interface RouteCase {
  name: string
  path: string
  pattern: string
  element: ReactElement
  routes?: Parameters<typeof mockApi>[0]
}

const CASES: RouteCase[] = [
  {
    name: '/revenue',
    path: '/revenue',
    pattern: '/revenue',
    element: <Revenue />,
  },
  {
    name: '/proxy',
    path: '/proxy',
    pattern: '/proxy',
    element: <ProxyMetrics />,
  },
  {
    name: '/proxy-logs',
    path: '/proxy-logs',
    pattern: '/proxy-logs',
    element: <ProxyLogs />,
  },
  {
    name: '/proxy-logs (with rows)',
    path: '/proxy-logs',
    pattern: '/proxy-logs',
    element: <ProxyLogs />,
    routes: {
      'GET /proxy-logs': () =>
        jsonResponse({
          logs: [1, 2, 3].map((id) => ({
            id,
            host: 'app.example.test',
            method: 'GET',
            path: `/page-${id}`,
            status_code: 200,
            routing_status: 'routed',
            request_source: 'proxy',
            request_id: `req-${id}`,
            is_system_request: false,
            timestamp: '2026-10-12T10:00:00Z',
            response_time_ms: 12,
            client_ip: '203.0.113.7',
            browser: 'Firefox',
            device_type: 'desktop',
            is_bot: false,
          })),
          page: 1,
          page_size: 20,
          total: 3,
          total_pages: 1,
        }),
    },
  },
  {
    name: '/traces/global/:traceId (with spans)',
    path: '/traces/global/trace-1',
    pattern: '/traces/global/:traceId',
    element: <CrossProjectTraceDetail />,
    routes: {
      'GET /otel/global/traces/trace-1': () => jsonResponse(TRACE),
    },
  },
  {
    name: '/revenue (with events)',
    path: '/revenue',
    pattern: '/revenue',
    element: <Revenue />,
    routes: {
      'GET /revenue/events': () =>
        jsonResponse(
          [1, 2].map((id) => ({
            id,
            project_id: 1,
            project_name: 'Example',
            event_type: 'invoice.paid',
            amount_minor: 1500 * id,
            currency: 'usd',
            occurred_at: '2026-10-12T10:00:00Z',
            customer_ref: `cus_${id}`,
            mrr_minor: 1500,
          }))
        ),
      'GET /projects': () =>
        jsonResponse({
          projects: [{ id: 1, name: 'Example', slug: 'example' }],
          total: 1,
          page: 1,
          per_page: 20,
          total_pages: 1,
        }),
    },
  },
  {
    name: '/workspaces',
    path: '/workspaces',
    pattern: '/workspaces',
    element: <Sandboxes key="workspaces" workspacesOnly />,
  },
  {
    name: '/sandboxes',
    path: '/sandboxes',
    pattern: '/sandboxes',
    element: <Sandboxes />,
  },
  {
    name: '/workspaces/:workspaceId',
    path: '/workspaces/ws-1',
    pattern: '/workspaces/:workspaceId',
    element: <WorkspaceDetail />,
  },
  {
    name: '/setup/ai',
    path: '/setup/ai',
    pattern: '/setup/ai',
    element: <AiOnboarding />,
  },
  {
    name: '/traces/global/:traceId',
    path: '/traces/global/trace-1',
    pattern: '/traces/global/:traceId',
    element: <CrossProjectTraceDetail />,
  },
]

async function violationsFor(
  initialPath: string,
  routes: ReactElement,
  api: Parameters<typeof mockApi>[0] = {}
): Promise<string[]> {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, retryOnMount: false } },
  })
  const mock = mockApi(api)
  const view = render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: {
            id: 1,
            name: 'Example User',
            username: 'example',
            avatar_url: '',
            mfa_enabled: false,
            role: 'admin',
          },
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <BreadcrumbProvider>
          <MemoryRouter initialEntries={[initialPath]}>{routes}</MemoryRouter>
        </BreadcrumbProvider>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 60))
  })
  const result = await axe.run(view.container, AXE_OPTIONS)
  mock.restore()
  client.clear()
  return result.violations.map(
    (v) => `${v.id}: ${v.nodes.map((n) => n.html.slice(0, 160)).join(' | ')}`
  )
}

for (const entry of CASES) {
  test(`${entry.name} has no accessibility violations`, async () => {
    expect(
      await violationsFor(
        entry.path,
        <Routes>
          <Route path={entry.pattern} element={entry.element} />
        </Routes>,
        entry.routes
      )
    ).toEqual([])
  })
}

for (const section of MONITORING_SECTIONS) {
  test(`/monitoring/${section.id} has no accessibility violations`, async () => {
    expect(
      await violationsFor(
        `/monitoring/${section.id}`,
        <Routes>
          <Route path="/monitoring" element={<Monitoring />}>
            <Route path=":section" element={<MonitoringSettings />} />
          </Route>
        </Routes>
      )
    ).toEqual([])
  })
}
