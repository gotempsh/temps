// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import type {
  EgressActivityPage,
  EgressActivityRecord,
  EgressRecordingStatus,
} from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  describeDestination,
  environmentEgress,
  EXAMPLE_RECORDS,
  formatDuration,
  outcomeVariant,
  sandboxEgress,
} from './egress-recording'
import { OutboundActivityCard } from './OutboundActivityCard'

installBrowserWindow()
afterEach(cleanup)

const settle = () => new Promise((resolve) => setTimeout(resolve, 100))

function status(enabled: boolean): EgressRecordingStatus {
  return {
    workload_kind: 'sandbox',
    workload_id: 'sbx_1',
    enabled,
    configured: true,
    reason_code: null,
    reason: null,
    setup_path: null,
    nodes: [
      {
        node_id: 0,
        node_name: 'control-plane',
        recording: true,
        reason_code: null,
        reason: null,
        setup_path: null,
      },
    ],
    project_id: 7,
    logs_path: null,
  }
}

function record(over: Partial<EgressActivityRecord>): EgressActivityRecord {
  return {
    timestamp: '2026-11-25T10:00:00Z',
    outcome: 'allowed',
    reason: null,
    kind: 'tunnel',
    host: 'api.example.com',
    port: 443,
    peer_address: null,
    method: null,
    path: null,
    relay: null,
    bytes_sent: 1300,
    bytes_received: 4600,
    duration_ms: 230,
    count: 1,
    last_started_at: null,
    node_name: 'control-plane',
    trace_id: null,
    span_id: null,
    ...over,
  }
}

function page(
  records: EgressActivityRecord[],
  total = records.length
): EgressActivityPage {
  return {
    workload_kind: 'sandbox',
    workload_id: 'sbx_1',
    records,
    page: 1,
    per_page: 20,
    total,
    has_more: total > records.length,
  }
}

function view(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <MemoryRouter>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </MemoryRouter>
  )
}

test('lists the sandbox own recorded connections', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(status(true)),
    'GET /v1/sandboxes/sbx_1/egress-records': () =>
      jsonResponse(
        page([
          record({}),
          record({
            host: 'blocked.example.com',
            outcome: 'refused',
            reason: 'no_allow_rule',
            count: 3,
          }),
        ])
      ),
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  expect(card.getByText('CONNECT api.example.com:443')).toBeTruthy()
  expect(card.getByText('CONNECT blocked.example.com:443')).toBeTruthy()
  expect(card.getByText('no_allow_rule')).toBeTruthy()
  expect(card.getByText('×3')).toBeTruthy()
  expect(card.queryByRole('status')).toBeNull()
  api.restore()
})

test('with recording off and nothing recorded, it onboards', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(status(false)),
    'GET /v1/sandboxes/sbx_1/egress-records': () => jsonResponse(page([])),
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  const notice = card.getByRole('status')
  expect(notice.textContent).toContain('Recording is off')
  expect(notice.textContent).toContain('Record outbound connections')
  expect(notice.textContent).toContain(EXAMPLE_RECORDS[0])
  api.restore()
})

test('a failed load says what failed', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(status(true)),
    'GET /v1/sandboxes/sbx_1/egress-records': () =>
      jsonResponse(
        {
          title: 'OpenTelemetry Storage Unavailable',
          detail: 'OpenTelemetry storage is not enabled on this control plane',
          status: 503,
        },
        503
      ),
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  expect(card.getByText('Could not load outbound activity')).toBeTruthy()
  api.restore()
})

test('an environment lists its records from its own endpoint', async () => {
  let asked = ''
  const api = mockApi({
    'GET /projects/7/environments/21/egress-recording': () =>
      jsonResponse(status(false)),
    'GET /projects/7/environments/21/egress-records': (request) => {
      asked = new URL(request.url).search
      return jsonResponse(page([record({ node_name: 'control-plane' })]))
    },
  })
  const card = view(
    <OutboundActivityCard workload={environmentEgress(7, 21)} />
  )
  await settle()
  expect(card.getByText('CONNECT api.example.com:443')).toBeTruthy()
  expect(asked).toContain('page=1')
  expect(asked).toContain('per_page=20')
  api.restore()
})

test('an environment with recording off onboards in its own words', async () => {
  const api = mockApi({
    'GET /projects/7/environments/21/egress-recording': () =>
      jsonResponse(status(false)),
    'GET /projects/7/environments/21/egress-records': () =>
      jsonResponse(page([])),
  })
  const card = view(
    <OutboundActivityCard workload={environmentEgress(7, 21)} />
  )
  await settle()
  expect(card.getByRole('status').textContent).toContain(
    'every connection this environment opens'
  )
  api.restore()
})

test('destinations read as the workload asked for them', () => {
  expect(describeDestination(record({}))).toBe('CONNECT api.example.com:443')
  expect(
    describeDestination(
      record({ kind: 'forward', port: 80, method: 'POST', path: '/v1/items' })
    )
  ).toBe('POST api.example.com/v1/items')
  expect(
    describeDestination(
      record({ kind: 'forward', port: 8080, method: 'GET', path: '/' })
    )
  ).toBe('GET api.example.com:8080/')
  expect(describeDestination(record({ kind: 'relay', relay: 'model' }))).toBe(
    'Temps model relay'
  )
  expect(outcomeVariant('refused')).toBe('destructive-subtle')
  expect(outcomeVariant('allowed')).toBe('success-subtle')
  expect(formatDuration(230)).toBe('230 ms')
  expect(formatDuration(1400)).toBe('1.4 s')
  expect(formatDuration(125_000)).toBe('2 min 5 s')
})
