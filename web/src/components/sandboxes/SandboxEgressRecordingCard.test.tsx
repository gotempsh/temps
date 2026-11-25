// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import type { EgressRecordingStatus } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { canToggle, EXAMPLE_RECORDS } from './egress-recording'
import { SandboxEgressRecordingCard } from './SandboxEgressRecordingCard'

installBrowserWindow()
afterEach(cleanup)

const settle = () => new Promise((resolve) => setTimeout(resolve, 100))

function status(over: Partial<EgressRecordingStatus>): EgressRecordingStatus {
  return {
    sandbox_id: 'sbx_1',
    enabled: false,
    configured: true,
    reason_code: null,
    reason: null,
    setup_path: null,
    node_id: 3,
    node_name: 'worker-a',
    project_id: 7,
    logs_path:
      '/projects/agent/telemetry-logs?attribute_key=temps.sandbox.id&attribute_value=sbx_1',
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
  const card = view(<SandboxEgressRecordingCard sandboxId="sbx_1" />)
  await settle()
  const toggle = card.getByRole('switch')
  expect(toggle.hasAttribute('disabled')).toBe(false)
  fireEvent.click(toggle)
  await settle()
  expect(body).toEqual({ enabled: true })
  const link = card.getByRole('link', { name: 'Open recorded connections' })
  expect(link.getAttribute('href')).toContain(
    'attribute_key=temps.sandbox.id&attribute_value=sbx_1'
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
          setup_path: '/settings/nodes/3',
        })
      ),
  })
  const card = view(<SandboxEgressRecordingCard sandboxId="sbx_1" />)
  await settle()
  expect(card.getByRole('switch').hasAttribute('disabled')).toBe(true)
  const notice = card.getByRole('status')
  expect(notice.textContent).toContain(
    'Recording is not available for this sandbox'
  )
  expect(notice.textContent).toContain('the node runs macos')
  expect(notice.textContent).toContain(EXAMPLE_RECORDS[0])
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
