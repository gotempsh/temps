// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { groupHref } from '@/lib/terminology'
import { InheritedVariablesPanel } from './InheritedVariablesPanel'

installBrowserWindow()
afterEach(cleanup)

const group = { id: 4, name: 'Storefront', slug: 'storefront' }

function renderPanel(
  variables: {
    key: string
    environment_slug: string | null
    is_secret: boolean
    defined_on_project: boolean
  }[]
) {
  mockApi({
    'GET /projects/7/inherited-env-vars': () =>
      jsonResponse({ group, variables }),
  })
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <InheritedVariablesPanel project={{ id: 7 } as ProjectResponse} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a project with no shared variables is one line that leads to adding one', async () => {
  const view = renderPanel([])

  const add = await view.findByRole('link', { name: 'Add a shared variable' })
  expect(add.getAttribute('href')).toBe(
    `${groupHref(group.slug)}?tab=variables`
  )
  expect(view.getByText(/every service in Storefront/)).toBeTruthy()
  // Nothing is inherited, so no card, heading or second link to the same place.
  expect(view.queryByText(/Inherited from project/)).toBeNull()
  expect(
    view.queryByRole('link', { name: 'Manage shared variables' })
  ).toBeNull()
  expect(view.container.querySelector('.border')).toBeNull()
})

test('shared variables keep the card, the override and the way to manage them', async () => {
  const view = renderPanel([
    {
      key: 'API_URL',
      environment_slug: null,
      is_secret: false,
      defined_on_project: true,
    },
  ])

  await view.findByText('API_URL')
  expect(view.getByText(/Inherited from project/)).toBeTruthy()
  expect(view.getByText('All environments')).toBeTruthy()
  expect(
    view.getByText("Overridden by this service's own API_URL")
  ).toBeTruthy()
  expect(
    view.getByRole('link', { name: 'Manage shared variables' })
  ).toBeTruthy()
  expect(view.queryByRole('link', { name: 'Add a shared variable' })).toBeNull()
})
