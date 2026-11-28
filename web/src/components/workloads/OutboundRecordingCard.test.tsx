// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import type { EgressRecordingNode, EgressRecordingStatus } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  canToggle,
  environmentEgress,
  EXAMPLE_RECORDS,
  sandboxEgress,
  toggleToast,
} from './egress-recording'
import { OutboundRecordingCard } from './OutboundRecordingCard'

installBrowserWindow()
afterEach(cleanup)

const settle = () => new Promise((resolve) => setTimeout(resolve, 100))

function node(over: Partial<EgressRecordingNode>): EgressRecordingNode {
  return {
    node_id: 3,
    node_name: 'worker-a',
    recording: true,
    reason_code: null,
    reason: null,
    setup_path: null,
    ...over,
  }
}

function status(over: Partial<EgressRecordingStatus>): EgressRecordingStatus {
  return {
    workload_kind: 'sandbox',
    workload_id: 'sbx_1',
    enabled: false,
    configured: true,
    reason_code: null,
    reason: null,
    setup_path: null,
    nodes: [node({})],
    project_id: 7,
    logs_path:
      '/projects/agent/telemetry-logs?workload_kind=sandbox&workload_id=1',
    ...over,
  }
}

function view(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <MemoryRouter>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </MemoryRouter>
  )
}

test('turning recording on sends the switch and links to the records', async () => {
  let body: unknown = null
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () => jsonResponse(status({})),
    'PUT /v1/sandboxes/sbx_1/egress-recording': async (request) => {
      body = await request.json()
      return jsonResponse(status({ enabled: true }))
    },
  })
  const card = view(<OutboundRecordingCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  const toggle = card.getByRole('switch')
  expect(toggle.hasAttribute('disabled')).toBe(false)
  fireEvent.click(toggle)
  await settle()
  expect(body).toEqual({ enabled: true })
  const link = card.getByRole('link', { name: 'Open recorded connections' })
  expect(link.getAttribute('href')).toContain(
    'workload_kind=sandbox&workload_id=1'
  )
  api.restore()
})

test('a node that cannot record onboards instead of hiding the option', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(
        status({
          configured: false,
          reason_code: 'unsupported_platform',
          reason:
            'the node runs macos; outbound connections are recorded by the per-node egress proxy, which records only on Linux nodes',
          setup_path: null,
        })
      ),
  })
  const card = view(<OutboundRecordingCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  expect(card.getByRole('switch').hasAttribute('disabled')).toBe(true)
  const notice = card.getByRole('status')
  expect(notice.textContent).toContain(
    'Recording is not available for this sandbox'
  )
  expect(notice.textContent).toContain('the node runs macos')
  expect(notice.textContent).toContain(EXAMPLE_RECORDS[0])
  expect(
    card.queryByRole('link', { name: /in Nodes/ }),
    'nothing on the node page fixes a platform'
  ).toBeNull()
  api.restore()
})

test('a node whose agent is not reporting links to the node', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/sbx_1/egress-recording': () =>
      jsonResponse(
        status({
          configured: false,
          reason_code: 'node_not_reporting',
          reason: "the node's agent has not reported in the last 120 seconds",
          setup_path: '/settings/nodes/3',
          nodes: [
            node({
              recording: false,
              reason_code: 'node_not_reporting',
              setup_path: '/settings/nodes/3',
            }),
          ],
        })
      ),
  })
  const card = view(<OutboundRecordingCard workload={sandboxEgress('sbx_1')} />)
  await settle()
  expect(
    card
      .getByRole('link', { name: 'Open worker-a in Nodes' })
      .getAttribute('href')
  ).toBe('/settings/nodes/3')
  api.restore()
})

test('recording that is on but cannot work says so and can still be turned off', () => {
  const broken = status({
    enabled: true,
    configured: false,
    reason: 'the node is not reporting',
  })
  expect(canToggle(broken, false)).toBe(true)
  expect(canToggle(status({ configured: false }), false)).toBe(false)
  expect(canToggle(status({}), true)).toBe(false)
})

test('an environment uses its own endpoints and says when recording starts', async () => {
  let body: unknown = null
  const api = mockApi({
    'GET /projects/7/environments/21/egress-recording': () =>
      jsonResponse(
        status({
          workload_kind: 'deployment',
          workload_id: '21',
          nodes: [
            node({ node_id: 0, node_name: 'control-plane' }),
            node({
              recording: false,
              reason_code: 'proxy_not_serving',
              reason: "the node's egress proxy is not serving",
            }),
          ],
        })
      ),
    'PUT /projects/7/environments/21/egress-recording': async (request) => {
      body = await request.json()
      return jsonResponse(
        status({
          workload_kind: 'deployment',
          workload_id: '21',
          enabled: true,
        })
      )
    },
  })
  const card = view(
    <OutboundRecordingCard workload={environmentEgress(7, 21)} />
  )
  await settle()
  expect(card.container.textContent).toContain(
    'Log every connection this environment opens'
  )
  expect(card.container.textContent).toContain(
    'Applies to containers started after it is turned on'
  )
  const unrecorded = card.getByRole('list', {
    name: 'Nodes that do not record',
  })
  expect(unrecorded.textContent).toContain(
    "Not recorded on worker-a: the node's egress proxy is not serving"
  )
  expect(unrecorded.textContent).not.toContain('control-plane')
  fireEvent.click(card.getByRole('switch'))
  await settle()
  expect(body).toEqual({ enabled: true })
  api.restore()
})

test('a workload that is gone says so instead of offering the switch', () => {
  const card = view(
    <OutboundRecordingCard
      workload={sandboxEgress('sbx_1')}
      disabled
      disabledNote="A destroyed sandbox makes no connections."
    />
  )
  expect(card.queryByRole('switch')).toBeNull()
  expect(card.container.textContent).toContain(
    'A destroyed sandbox makes no connections.'
  )
})

test('a switch that did not take answers with a warning, not "now recorded"', () => {
  const reason = 'the sandbox egress proxy could not be switched yet'
  expect(
    toggleToast(
      status({
        enabled: true,
        configured: false,
        reason_code: 'not_applied',
        reason,
      })
    )
  ).toEqual({
    kind: 'warning',
    title: 'Recording is on, but nothing is recorded yet',
    description: reason,
  })
  expect(toggleToast(status({ enabled: true })).title).toBe(
    'Outbound connections are now recorded'
  )
  expect(toggleToast(status({ enabled: false })).kind).toBe('success')
})
