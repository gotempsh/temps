// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, render, waitFor } from '@testing-library/react'
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

const PREFERENCES = {
  backup_failures_enabled: true,
  backup_successes_enabled: false,
  batch_similar_notifications: false,
  build_errors_enabled: true,
  deployment_failures_enabled: true,
  digest_sections: {
    deployments: true,
    errors: true,
    funnels: true,
    performance: true,
    projects: true,
  },
  digest_send_day: 'monday',
  digest_send_time: '09:00',
  dns_changes_enabled: true,
  domain_expiration_enabled: true,
  email_enabled: true,
  error_threshold: 10,
  error_time_window: 60,
  load_balancer_issues_enabled: true,
  minimum_severity: 'warning',
  retention_policy_violations_enabled: true,
  route_downtime_enabled: true,
  runtime_errors_enabled: true,
  s3_connection_issues_enabled: true,
  slack_enabled: false,
  ssl_days_before_expiration: 30,
  ssl_expiration_enabled: true,
  weekly_digest_enabled: false,
}

const APPLICATION = {
  created_at: '2026-10-12T10:00:00Z',
  description: null,
  name: 'Example workspace',
  projects: [],
  public_id: 'ws-1',
  status: 'active',
  updated_at: '2026-10-12T10:00:00Z',
}

const NO_PROJECTS = {
  projects: [],
  total: 0,
  page: 1,
  per_page: 20,
  total_pages: 0,
}

const WORKSPACE = {
  cpu_limit: 2,
  data_network_service_count: 0,
  desired_state: 'running',
  disk_limit_enforced: true,
  disk_limit_mb: 10240,
  idle_timeout_secs: 900,
  memory_limit_mb: 4096,
  open_preview_ports: [],
  persistent_volume_healthy: true,
  pids_limit: 512,
  runtime: 'default',
  runtime_update_available: false,
  state: 'running',
}

const FEATURES = {
  backups_local: true,
  backups_remote: false,
  deployments_local: true,
  docker: true,
  image_builds_local: true,
  imports: true,
  kv: true,
  log_aggregation: true,
  managed_services: true,
  persistent_workspaces: true,
  profile: 'full',
  sandboxes: true,
  vulnerability_scanning: false,
}

/**
 * Copy of the error states these pages fall back to. A page sitting on one of
 * these would pass axe while hiding the controls the test exists to check, so
 * a case must mock the replies that carry it past them -- or say, with
 * `errorState`, that the error state itself is what is under test.
 */
const FAILURE_COPY = [
  'Failed to load',
  'Could not load',
  'could not be loaded',
  'unavailable',
]

interface RouteCase {
  name: string
  path: string
  pattern: string
  element: ReactElement
  routes?: Parameters<typeof mockApi>[0]
  /** Text that must be on screen before axe runs, so the real controls are checked. */
  ready?: string
  /** The case renders an error state on purpose. */
  errorState?: boolean
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
    routes: {
      'GET /projects': () => jsonResponse(NO_PROJECTS),
      'GET /nodes/0/metrics': () => jsonResponse([]),
    },
    ready: 'No proxy metrics yet',
  },
  {
    name: '/proxy-logs',
    path: '/proxy-logs',
    pattern: '/proxy-logs',
    element: <ProxyLogs />,
    routes: {
      'GET /proxy-logs': () =>
        jsonResponse({
          logs: [],
          page: 1,
          page_size: 20,
          total: 0,
          total_pages: 0,
        }),
    },
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
    routes: {
      'GET /platform/features': () => jsonResponse(FEATURES),
      'GET /ai/applications': () => jsonResponse([APPLICATION]),
      'GET /ai/workspace': () => jsonResponse(WORKSPACE),
      'GET /ai/workspace-activity': () => jsonResponse({ workspaces: [] }),
      'GET /ai/applications/ws-1/workspace': () => jsonResponse(WORKSPACE),
    },
    ready: 'Example workspace',
  },
  {
    name: '/sandboxes',
    path: '/sandboxes',
    pattern: '/sandboxes',
    element: <Sandboxes />,
    routes: {
      'GET /platform/features': () => jsonResponse(FEATURES),
      'GET /v1/sandboxes': () =>
        jsonResponse({ pagination: { count: 0 }, sandboxes: [] }),
    },
  },
  {
    name: '/workspaces/:workspaceId',
    path: '/workspaces/ws-1',
    pattern: '/workspaces/:workspaceId',
    element: <WorkspaceDetail />,
    routes: {
      'GET /ai/applications/ws-1': () => jsonResponse(APPLICATION),
      'GET /ai/applications/ws-1/workspace': () => jsonResponse(WORKSPACE),
    },
    ready: 'No linked services.',
  },
  {
    name: '/setup/ai',
    path: '/setup/ai',
    pattern: '/setup/ai',
    element: <AiOnboarding />,
  },
  {
    name: '/traces/global/:traceId (trace not found)',
    path: '/traces/global/trace-1',
    pattern: '/traces/global/:traceId',
    element: <CrossProjectTraceDetail />,
    errorState: true,
  },
]

async function violationsFor(
  initialPath: string,
  routes: ReactElement,
  api: Parameters<typeof mockApi>[0] = {},
  options: { ready?: string; errorState?: boolean } = {}
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
  if (options.ready) {
    const { ready } = options
    await waitFor(() => expect(view.container.textContent).toContain(ready))
  }
  // A page sitting on its error state would pass axe while hiding the controls.
  const text = view.container.textContent ?? ''
  const failed = options.errorState
    ? []
    : FAILURE_COPY.filter((copy) => text.includes(copy))
  const result = await axe.run(view.container, AXE_OPTIONS)
  mock.restore()
  client.clear()
  return [
    ...failed.map((copy) => `page rendered its error state: "${copy}"`),
    ...result.violations.map(
      (v) => `${v.id}: ${v.nodes.map((n) => n.html.slice(0, 160)).join(' | ')}`
    ),
  ]
}

for (const entry of CASES) {
  test(`${entry.name} has no accessibility violations`, async () => {
    expect(
      await violationsFor(
        entry.path,
        <Routes>
          <Route path={entry.pattern} element={entry.element} />
        </Routes>,
        entry.routes,
        { ready: entry.ready, errorState: entry.errorState }
      )
    ).toEqual([])
  })
}

/** A control or heading each section only renders after preferences load. */
const SECTION_READY: Record<
  (typeof MONITORING_SECTIONS)[number]['id'],
  string
> = {
  alerts: 'Route monitoring',
  rules: 'Monitoring & alerts',
  alarms: 'Monitoring & alerts',
  notifications: 'Weekly digest',
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
        </Routes>,
        {
          'GET /notification-preferences': () => jsonResponse(PREFERENCES),
          'GET /projects': () => jsonResponse(NO_PROJECTS),
          'GET /nodes/0/metrics/alert-rules': () => jsonResponse([]),
        },
        { ready: SECTION_READY[section.id] }
      )
    ).toEqual([])
  })
}
