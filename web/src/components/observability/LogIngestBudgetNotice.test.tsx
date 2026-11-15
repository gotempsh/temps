// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type {
  LogIngestBudgetStatus,
  ThrottledLogSource,
} from '@/api/client/types.gen'
import { LogIngestBudgetNotice } from './LogIngestBudgetNotice'

const budget: LogIngestBudgetStatus = {
  lines_per_sec: 250,
  bytes_per_sec: 256 * 1024,
  burst_seconds: 10,
  setup_path: '/settings/metrics-monitoring#container-log-ingest-budget',
  throttled: [],
}

const noisy: ThrottledLogSource = {
  container_id: 'a1b2c3d4e5f6a7b8c9d0',
  service: 'worker',
  project_id: 4,
  env: '1',
  offered_lines_per_sec: 1200,
  offered_bytes_per_sec: 96 * 1024,
  dropped_lines: 12345,
  dropped_bytes: 900_000,
  reason: 'lines_per_sec',
  throttled_since: '2026-01-01T00:00:00Z',
  last_dropped_at: '2026-01-01T00:01:00Z',
  lines_per_sec_limit: 250,
  bytes_per_sec_limit: 256 * 1024,
  limit_overridden: false,
}

const render = (
  value: LogIngestBudgetStatus | undefined,
  projectLabels?: Record<string, string>
) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <LogIngestBudgetNotice budget={value} projectLabels={projectLabels} />
    </MemoryRouter>
  )

test('says nothing while every container is within its budget', () => {
  expect(render(budget)).toBe('')
  expect(render(undefined)).toBe('')
})

test('names the throttled container, its rate and what was dropped', () => {
  const markup = render({ ...budget, throttled: [noisy] }, { '4': 'shop' })
  expect(markup).toContain(
    '1 container is logging faster than its ingestion budget'
  )
  expect(markup).toContain('worker (shop)')
  expect(markup).toContain('a1b2c3d4e5f6')
  expect(markup).toContain('~1,200 lines/s (96.0 KiB/s)')
  expect(markup).toContain('12,345 lines dropped')
  expect(markup).toContain('250 lines/s and 256.0 KiB/s')
})

test('a source on its own budget shows that budget, not the platform one', () => {
  const inherited = render({ ...budget, throttled: [noisy] })
  expect(inherited).not.toContain('own budget:')

  const overridden = render({
    ...budget,
    throttled: [
      {
        ...noisy,
        lines_per_sec_limit: 50,
        bytes_per_sec_limit: 0,
        limit_overridden: true,
      },
    ],
  })
  expect(overridden).toContain('own budget: 50 lines/s')
  expect(overridden).not.toContain('own budget: 50 lines/s and')
})

test('links straight to the setting that changes the budget', () => {
  const markup = render({ ...budget, throttled: [noisy] })
  expect(markup).toContain(
    'href="/settings/metrics-monitoring#container-log-ingest-budget"'
  )
})

test('an external service on a worker node is named by service and node', () => {
  const markup = render({
    ...budget,
    throttled: [
      {
        ...noisy,
        project_id: undefined,
        external_service_id: 7,
        service: 'postgres-main',
        node_name: 'worker-2',
        offered_lines_per_sec: 0,
        offered_bytes_per_sec: 0,
      },
    ],
  })
  expect(markup).toContain('postgres-main (database service) on worker-2')
  expect(markup).toContain('quiet now')
})

test('lists the noisiest few and counts the rest', () => {
  const many = Array.from({ length: 7 }, (_, i) => ({
    ...noisy,
    container_id: `container-${i}`,
  }))
  const markup = render({ ...budget, throttled: many })
  expect(markup).toContain(
    '7 containers are logging faster than their ingestion budget'
  )
  expect(markup).toContain('and 2 more.')
  expect(markup).not.toContain('container-5')
})

test("a project's log view lists only that project's containers", () => {
  const theirs = { ...noisy, container_id: 'other', project_id: 9 }
  const database = {
    ...noisy,
    container_id: 'db',
    project_id: undefined,
    external_service_id: 4,
  }
  const value = { ...budget, throttled: [noisy, theirs, database] }

  const project = renderToStaticMarkup(
    <MemoryRouter>
      <LogIngestBudgetNotice budget={value} scope={{ projectId: 4 }} />
    </MemoryRouter>
  )
  expect(project).toContain('1 container is logging')
  expect(project).toContain('a1b2c3d4e5f6')
  expect(project).not.toContain('<code class="text-[11px]">other')

  // External service 4 is not project 4.
  const service = renderToStaticMarkup(
    <MemoryRouter>
      <LogIngestBudgetNotice budget={value} scope={{ externalServiceId: 4 }} />
    </MemoryRouter>
  )
  expect(service).toContain('1 container is logging')
  expect(service).toContain('<code class="text-[11px]">db</code>')

  const quiet = renderToStaticMarkup(
    <MemoryRouter>
      <LogIngestBudgetNotice budget={value} scope={{ projectId: 99 }} />
    </MemoryRouter>
  )
  expect(quiet).toBe('')
})

test('a service-scoped notice links to that service’s own log settings', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <LogIngestBudgetNotice
        budget={{
          ...budget,
          throttled: [
            { ...noisy, project_id: undefined, external_service_id: 9 },
          ],
        }}
        scope={{ externalServiceId: 9 }}
      />
    </MemoryRouter>
  )
  expect(html).toContain('href="/storage/9/logs?settings=log-ingestion"')
  expect(html).toContain('change this service&#x27;s ingestion budget')
  expect(html).not.toContain(budget.setup_path)
})

test('an unscoped notice still links to the platform budget', () => {
  const html = render({ ...budget, throttled: [noisy] })
  expect(html).toContain(
    'href="/settings/metrics-monitoring#container-log-ingest-budget"'
  )
})
