// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import ApiKeyCreate from './ApiKeyCreate'

installBrowserWindow()
afterEach(cleanup)

const PERMISSIONS = {
  permissions: [
    {
      category: 'Projects',
      name: 'projects:read',
      description: 'Read projects',
    },
    {
      category: 'Projects',
      name: 'projects:write',
      description: 'Change projects',
    },
    { category: 'Domains', name: 'domains:read', description: 'Read domains' },
  ],
  roles: [],
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/keys/new']}>
        <ApiKeyCreate />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

async function toPermissionsStep(view: ReturnType<typeof renderPage>) {
  const name = view.container.querySelector<HTMLInputElement>('#name')
  expect(name).not.toBeNull()
  fireEvent.change(name!, { target: { value: 'ci-key' } })
  fireEvent.click(view.getByRole('button', { name: 'Next: Permissions' }))
  await view.findByText('Custom permissions')
  await view.findByLabelText('Select all Projects permissions')
}

// A trigger that is a `div` is not focusable and puts `aria-expanded` on an
// element that may not carry it; the select-all checkbox inside it was also a
// control nested in another control.
test('each permission category is a real button, beside its select-all checkbox', async () => {
  const api = mockApi({
    'GET /api-keys/permissions': () => jsonResponse(PERMISSIONS),
  })
  const view = renderPage()
  await toPermissionsStep(view)

  const expanders = Array.from(
    view.container.querySelectorAll('[aria-expanded]')
  )
  expect(expanders.length).toBeGreaterThanOrEqual(2)
  for (const expander of expanders) {
    expect(expander.tagName).toBe('BUTTON')
    expect(expander.querySelector('[role="checkbox"]')).toBeNull()
  }

  const selectAll = view.getByLabelText('Select all Projects permissions')
  expect(selectAll.closest('button[aria-expanded]')).toBeNull()
  api.restore()
})

test('the category button expands its permissions from the keyboard', async () => {
  const api = mockApi({
    'GET /api-keys/permissions': () => jsonResponse(PERMISSIONS),
  })
  const view = renderPage()
  await toPermissionsStep(view)

  const expander = view.getByRole('button', { name: /Projects/ })
  expect(expander.getAttribute('aria-expanded')).toBe('false')
  fireEvent.click(expander)
  expect(expander.getAttribute('aria-expanded')).toBe('true')
  expect(await view.findByText('projects:read')).toBeTruthy()
  api.restore()
})
