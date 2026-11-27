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
  activityPages,
  describeDestination,
  environmentEgress,
  EXAMPLE_RECORDS,
  formatDuration,
  outcomeVariant,
  pageAfterEmpty,
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
  // IPv6 literals keep their brackets, so the port stays apart.
  expect(describeDestination(record({ host: '2001:db8::1' }))).toBe(
    'CONNECT [2001:db8::1]:443'
  )
  expect(
    describeDestination(
      record({ kind: 'forward', host: '2001:db8::1', port: 80, path: '/' })
    )
  ).toBe('GET [2001:db8::1]/')
  expect(describeDestination(record({ kind: 'relay', relay: 'model' }))).toBe(
    'Temps model relay'
  )
  expect(outcomeVariant('refused')).toBe('destructive-subtle')
  expect(outcomeVariant('allowed')).toBe('success-subtle')
  expect(formatDuration(230)).toBe('230 ms')
  expect(formatDuration(1400)).toBe('1.4 s')
  expect(formatDuration(125_000)).toBe('2 min 5 s')
})

test('a later page emptied by retention goes back to the records left', async () => {
  // 21 records: page 2 holds the oldest. It expires while page 2 is open,
  // so page 2 comes back empty with 20 left.
  let expired = false
  const asked: string[] = []
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(status(true)),
    'GET /v1/sandboxes/sbx_1/egress-records': (request) => {
      const query = new URL(request.url).searchParams
      asked.push(query.get('page') ?? '')
      if (query.get('page') === '2') {
        expired = true
        return jsonResponse({ ...page([], 20), page: 2 })
      }
      const first = Array.from({ length: 20 }, (_, i) =>
        record({ host: `h${i}.example.com` })
      )
      return jsonResponse(page(first, expired ? 20 : 21))
    },
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  const next = card.getAllByRole('button', { name: /next/i })[0]
  next.click()
  await settle()
  await settle()
  expect(asked).toContain('2')
  expect(asked[asked.length - 1]).toBe('1')
  expect(card.getByText('CONNECT h0.example.com:443')).toBeTruthy()
  api.restore()
})

test('a page that says more follow offers the next one, whatever the count says', async () => {
  // The total is a recent count: it says 20 while a 21st record arrived.
  const asked: string[] = []
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(status(true)),
    'GET /v1/sandboxes/sbx_1/egress-records': (request) => {
      const query = new URL(request.url).searchParams
      asked.push(query.get('page') ?? '')
      if (query.get('page') === '2') {
        return jsonResponse({
          ...page([record({ host: 'oldest.example.com' })], 20),
          page: 2,
        })
      }
      const first = Array.from({ length: 20 }, (_, i) =>
        record({ host: `h${i}.example.com` })
      )
      return jsonResponse({ ...page(first, 20), has_more: true })
    },
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  const next = card.getAllByRole('button', { name: /next/i })[0]
  next.click()
  await settle()
  expect(asked).toContain('2')
  expect(card.getByText('CONNECT oldest.example.com:443')).toBeTruthy()
  api.restore()
})

test('a later page keeps its way back when the count lags behind', async () => {
  // The count says 19 while 21 records exist: page one says more follow,
  // page two holds one record and says none do.
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(status(true)),
    'GET /v1/sandboxes/sbx_1/egress-records': (request) => {
      const query = new URL(request.url).searchParams
      if (query.get('page') === '2') {
        return jsonResponse({
          ...page([record({ host: 'oldest.example.com' })], 19),
          page: 2,
          has_more: false,
        })
      }
      const first = Array.from({ length: 20 }, (_, i) =>
        record({ host: `h${i}.example.com` })
      )
      return jsonResponse({ ...page(first, 19), has_more: true })
    },
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  card.getAllByRole('button', { name: /next/i })[0].click()
  await settle()
  expect(card.getByText('CONNECT oldest.example.com:443')).toBeTruthy()
  expect(
    card.getAllByRole('button', { name: /previous/i }).length
  ).toBeGreaterThan(0)
  api.restore()
})

test('the pager never shows fewer records than the pages seen hold', () => {
  // Page 2 holds the 21st record while the count still says 19.
  expect(
    activityPages({
      total: 19,
      page: 2,
      pageSize: 20,
      shown: 1,
      hasMore: false,
    })
  ).toEqual({ total: 21, totalPages: 2 })
  // Page 1 is full and more follow while the count says 20.
  expect(
    activityPages({
      total: 20,
      page: 1,
      pageSize: 20,
      shown: 20,
      hasMore: true,
    })
  ).toEqual({ total: 21, totalPages: 2 })
  // An exact count is kept as it is.
  expect(
    activityPages({
      total: 45,
      page: 1,
      pageSize: 20,
      shown: 20,
      hasMore: true,
    })
  ).toEqual({ total: 45, totalPages: 3 })
  expect(
    activityPages({ total: 0, page: 1, pageSize: 20, shown: 0, hasMore: false })
  ).toEqual({ total: 0, totalPages: 1 })
})

test('an emptied deep page goes straight to the last page the count fills', () => {
  // Page 1000 emptied by retention, one page left: straight to page 1.
  expect(pageAfterEmpty({ total: 3, page: 1000, pageSize: 20 })).toBe(1)
  expect(pageAfterEmpty({ total: 0, page: 5, pageSize: 20 })).toBe(1)
  // Only the last page emptied: one back.
  expect(pageAfterEmpty({ total: 40, page: 3, pageSize: 20 })).toBe(2)
})

test('an unknown recording status never says recording is off', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse({ title: 'Unavailable' }, 503),
    'GET /v1/sandboxes/sbx_1/egress-records': () => jsonResponse(page([])),
  })
  const card = view(<OutboundActivityCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  expect(card.queryByText('Recording is off')).toBeNull()
  expect(card.getByText('No connections recorded yet.')).toBeTruthy()
  api.restore()
})
