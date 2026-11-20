// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import axe from 'axe-core'
import type { SpanRecord } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { buildSpanTree, flattenTree } from '../../utils/spanTree'
import { SpanWaterfall } from './SpanWaterfall'

installBrowserWindow()
afterEach(cleanup)

function span(id: string, parent: string | null, name: string): SpanRecord {
  return {
    attributes: {},
    duration_ms: 20,
    end_time: '2026-01-01T00:00:00.050Z',
    events: [],
    kind: 'internal',
    name,
    parent_span_id: parent,
    project_id: 1,
    resource: { service_name: 'api' },
    span_id: id,
    start_time: '2026-01-01T00:00:00.030Z',
    status_code: 'OK',
    status_message: '',
    trace_id: 'trace-1',
  } as unknown as SpanRecord
}

function renderWaterfall(onSelect: (id: string | null) => void = () => {}) {
  const flat = flattenTree(
    buildSpanTree([
      span('root', null, 'GET /orders'),
      span('child', 'root', 'SELECT orders'),
    ])
  )
  const start = new Date('2026-01-01T00:00:00.000Z').getTime()
  return render(
    <SpanWaterfall
      flatSpans={flat}
      traceStart={start}
      traceEnd={start + 100}
      traceDuration={100}
      selectedSpanId={null}
      onSelect={onSelect}
    />
  )
}

test('the span tree has no nested interactive controls', async () => {
  const view = renderWaterfall()
  const result = await axe.run(view.container, {
    runOnly: {
      type: 'rule',
      values: ['nested-interactive', 'button-name', 'aria-allowed-attr'],
    },
  })
  expect(result.violations.map((v) => `${v.id}: ${v.nodes.length}`)).toEqual([])
  for (const row of view.container.querySelectorAll('button')) {
    expect(
      row.querySelector('button, [role="button"], a, [tabindex]')
    ).toBeNull()
  }
})

test('the collapse toggle is a named button that does not select the span', () => {
  const onSelect = mock((_id: string | null) => {})
  const view = renderWaterfall(onSelect)

  const toggle = view.getByRole('button', { name: 'Collapse span' })
  expect(toggle.getAttribute('aria-expanded')).toBe('true')
  expect(view.queryByText('SELECT orders')).not.toBeNull()

  fireEvent.click(toggle)
  expect(onSelect).not.toHaveBeenCalled()
  expect(view.queryByText('SELECT orders')).toBeNull()
  expect(
    view
      .getByRole('button', { name: 'Expand span' })
      .getAttribute('aria-expanded')
  ).toBe('false')
})
