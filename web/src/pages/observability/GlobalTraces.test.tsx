// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { GlobalTraceSummary } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { TraceRow } from './GlobalTraces'

installBrowserWindow()
afterEach(cleanup)

function trace(overrides: Partial<GlobalTraceSummary>): GlobalTraceSummary {
  return {
    trace_id: '0123456789abcdef0123456789abcdef',
    root_span_name: 'GET /checkout',
    start_time: '2026-01-01T00:00:00Z',
    duration_ms: 250,
    span_count: 3,
    error_count: 0,
    status_code: 'UNSET',
    project_id: 7,
    project_name: 'Example API',
    project_slug: 'example-api',
    ...overrides,
  } as GlobalTraceSummary
}

function renderRow(row: GlobalTraceSummary, slowestMs = 1000) {
  return render(
    <MemoryRouter>
      <table>
        <tbody>
          <TraceRow trace={row} slowestMs={slowestMs} projectImage={<span />} />
        </tbody>
      </table>
    </MemoryRouter>
  )
}

test('a trace is one row with no separate status column: a dot, spans and ID inline', () => {
  const view = renderRow(trace({}))
  const row = view.getByRole('row')

  // Trace, service, duration, started: status lives in the dot.
  expect(within(row).getAllByRole('cell')).toHaveLength(4)
  view.getByTitle(/Status not reported/)
  expect(row.textContent).toContain('3spans')
  expect(row.textContent).toContain('01234567')
  // An unset status is not a failure, so no error badge.
  expect(row.textContent).not.toMatch(/\d+ errors?/)
})

test('the trace name, waterfall and service are separate links, and the row itself is not one', () => {
  const view = renderRow(trace({}))
  const name = view.getByRole('link', { name: 'GET /checkout' })

  expect(name.getAttribute('href')).toStartWith(
    '/projects/example-api/traces/0123456789abcdef0123456789abcdef'
  )
  // DESIGN.md: no link overlay across the row. It would also stop the status
  // dot's explanation appearing on hover.
  expect(name.className).not.toContain('after:')
  const waterfall = view.getByRole('link', { name: 'Cross-service waterfall' })
  expect(waterfall.getAttribute('href')).toStartWith(
    '/traces/global/0123456789abcdef0123456789abcdef'
  )
  expect(view.getByRole('link', { name: /Example API/ })).toBeTruthy()
})

test('a failed trace shows a red dot and its error count', () => {
  const view = renderRow(trace({ error_count: 2, status_code: 'OK' }))
  view.getByTitle('Failed')
  expect(view.getByRole('row').textContent).toContain('2 errors')
})

test('the duration bar is scaled to the slowest trace and coloured by latency', () => {
  const view = renderRow(trace({ duration_ms: 250 }), 1000)
  const bar = view.container.querySelector<HTMLElement>('.bg-amber-500')

  expect(bar?.style.width).toBe('25%')
  expect(view.getByRole('row').textContent).toContain('250 ms')
})
