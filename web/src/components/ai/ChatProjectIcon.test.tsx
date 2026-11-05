// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ChatProjectIcon } from './ChatProjectIcon'

installBrowserWindow()
afterEach(cleanup)

const MEDIA_PATH = '/deployments/latest-media'

function icon() {
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <ChatProjectIcon projectId={7} name="example" />
    </QueryClientProvider>
  )
}

test('a project the user can read gets its logo', async () => {
  mockApi({
    'GET /projects/7': () =>
      jsonResponse({
        id: 7,
        name: 'example',
        source_type: 'git',
        service_template_image_url: '/presets/example.svg',
      }),
    [`GET ${MEDIA_PATH}`]: () => jsonResponse({ projects: {} }),
  })
  const view = icon()
  await waitFor(() =>
    expect(view.container.querySelector('img')?.getAttribute('src')).toBe(
      '/presets/example.svg'
    )
  )
})

test('a project the user lost access to never joins a media request', async () => {
  const api = mockApi({
    'GET /projects/7': () => jsonResponse({ title: 'Forbidden' }, 403),
  })
  const view = icon()
  await waitFor(() =>
    expect(api.calls.some((call) => call.path === '/projects/7')).toBe(true)
  )
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(api.calls.some((call) => call.path === MEDIA_PATH)).toBe(false)
  expect(view.container.textContent).toBe('E')
})
