// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { ComposePlacementPanel } from './ComposePlacementPanel'

installBrowserWindow()
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

test('a failed node list can be retried without leaving the settings panel', async () => {
  mockApi({
    'GET /projects/3/environments/7/compose-placement': () =>
      jsonResponse({ pinned: false }),
  })
  const retry = mock(() => {})
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <ComposePlacementPanel
        projectId={3}
        environmentId={7}
        nodes={[]}
        nodesLoaded={false}
        nodesError={new Error('Network unavailable')}
        onRetryNodes={retry}
      />
    </QueryClientProvider>
  )
  await view.findByText("Could not load the cluster's nodes")
  expect(
    view.getByRole('button', { name: 'Move stack' }).hasAttribute('disabled')
  ).toBe(true)
  const button = view.getByRole('button', { name: 'Try again' })
  expect(button.getAttribute('type')).toBe('button')
  fireEvent.click(button)
  expect(retry).toHaveBeenCalledTimes(1)
})

for (const status of ['active', 'offline', null]) {
  test(`pending-move alert describes a source with status ${status}`, async () => {
    mockApi({
      'GET /projects/3/environments/7/compose-placement': () =>
        jsonResponse({
          pinned: true,
          home_node_id: 4,
          home_node_name: 'worker-b',
          home_node_status: 'active',
          moving_from_node_id: 3,
          moving_from_node_name: 'worker-a',
          moving_from_node_status: status,
        }),
    })
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    const view = render(
      <QueryClientProvider client={client}>
        <ComposePlacementPanel
          projectId={3}
          environmentId={7}
          nodes={[]}
          nodesLoaded
          onRetryNodes={() => {}}
        />
      </QueryClientProvider>
    )
    const heading = await view.findByText(
      'Move pending: redeploy to complete it'
    )
    const alert = heading.closest('[role="alert"]')
    expect(alert).not.toBeNull()
    if (status === 'active') {
      expect(alert?.textContent).toContain('stops the stack on worker-a')
    } else {
      expect(alert?.textContent).toContain('running state there is unknown')
      expect(alert?.textContent).toContain(status ?? 'unavailable')
      expect(alert?.textContent).not.toContain('The next deployment stops')
    }
    expect(alert?.textContent).toContain('The data on worker-a stays there')
  })
}
