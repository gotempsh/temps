// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import type {
  DeploymentContainerLogContentResponse,
  DeploymentContainerLogsListResponse,
} from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DeploymentContainerLogs } from './DeploymentContainerLogs'

installBrowserWindow()
afterEach(cleanup)

const CAPTURED_AT = '2026-03-04T05:06:07Z'

const LIST: DeploymentContainerLogsListResponse = {
  logs: [
    {
      id: 11,
      deployment_id: 9,
      container_id: 'c0ffee',
      container_name: 'web-2',
      service_name: null,
      node_id: null,
      size_bytes: 2048,
      truncated: false,
      captured_at: CAPTURED_AT,
    },
  ],
}

const CONTENT: DeploymentContainerLogContentResponse = {
  id: 11,
  container_name: 'web-2',
  service_name: null,
  size_bytes: 2048,
  truncated: false,
  captured_at: CAPTURED_AT,
  content: 'listening on :3000',
}

test('captured logs show their ISO 8601 capture time and open their content', async () => {
  const api = mockApi({
    'GET /projects/3/deployments/9/container-logs': () => jsonResponse(LIST),
    'GET /projects/3/deployments/9/container-logs/11': () =>
      jsonResponse(CONTENT),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <DeploymentContainerLogs projectId={3} deploymentId={9} />
    </QueryClientProvider>
  )

  const row = await view.findByText('web-2')
  // The capture time is the server's ISO string, read as a real date (an
  // unreadable one would render "Invalid Date" or throw in date-fns).
  const when = view.container.querySelector('span[title]')
  expect(when?.getAttribute('title')).toContain('2026')
  expect(when?.textContent).toContain('ago')

  await act(async () => {
    fireEvent.click(row)
  })
  expect(await view.findByText('listening on :3000')).toBeTruthy()
  expect(api.called('GET', '/projects/3/deployments/9/container-logs/11')).toBe(
    true
  )
  api.restore()
})
