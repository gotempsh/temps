// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceIcon, type ServiceIconProps } from './ServiceIcon'

installBrowserWindow()
afterEach(cleanup)

const MEDIA = 'GET /deployments/latest-media'

function icon(service: ServiceIconProps['service']) {
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <ServiceIcon service={service} />
    </QueryClientProvider>
  )
}

/** Lets a request that was going to be made leave the batcher. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 20))

test('shows the latest deployment favicon of a project', async () => {
  const api = mockApi({
    [MEDIA]: () =>
      jsonResponse({
        projects: { 7: { url: 'https://app.example.test/' } },
      }),
  })
  const view = icon({ id: 7, name: 'Example' })
  await waitFor(() =>
    expect(view.container.querySelector('img')?.getAttribute('src')).toBe(
      'https://app.example.test/favicon.ico'
    )
  )
  expect(api.calls.map((call) => call.path)).toEqual([
    '/deployments/latest-media',
  ])
})

test('prefers the template image of a project', () => {
  mockApi({ [MEDIA]: () => jsonResponse({ projects: {} }) })
  const view = icon({
    id: 8,
    name: 'Example',
    service_template_image_url: '/presets/example.svg',
  })
  expect(view.container.querySelector('img')?.getAttribute('src')).toBe(
    '/presets/example.svg'
  )
})

test('a project with no deployment shows its initial, without a second label', async () => {
  mockApi({ [MEDIA]: () => jsonResponse({ projects: {} }) })
  const view = icon({ id: 9, name: 'example' })
  await settle()
  expect(view.container.textContent).toBe('E')
  expect(view.container.querySelector('[role="img"]')).toBeNull()
})

test('a failed media request still shows the initial', async () => {
  mockApi({ [MEDIA]: () => jsonResponse({ title: 'Boom' }, 500) })
  const view = icon({ id: 10, name: 'example' })
  await settle()
  expect(view.container.textContent).toBe('E')
})

test('asks for no media for an external project or one with no id', async () => {
  const api = mockApi({})
  icon({ id: 11, name: 'External', source_type: 'external' })
  icon({ name: 'No id' })
  await settle()
  expect(api.calls).toEqual([])
})
