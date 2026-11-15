// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
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

function renderPage(role?: string, entry = '/settings/keys/new') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={
          role === undefined
            ? undefined
            : {
                user: { id: 1, role } as unknown as UserResponse,
                isLoading: false,
                error: null,
                logout: async () => {},
                refetch: () => {},
              }
        }
      >
        <MemoryRouter initialEntries={[entry]}>
          <ApiKeyCreate />
        </MemoryRouter>
      </AuthContext.Provider>
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

const CEILING_CATALOGUE = {
  permissions: [
    { category: 'Domains', name: 'domains:read', description: 'Read domains' },
    {
      category: 'Domains',
      name: 'domains:write',
      description: 'Change domains',
    },
    { category: 'System', name: 'system:admin', description: 'Administer' },
  ],
  roles: [
    {
      name: 'admin',
      description: 'Full access',
      permissions: ['domains:read', 'domains:write', 'system:admin'],
    },
    {
      name: 'user',
      description: 'Standard access',
      permissions: ['domains:read', 'domains:write'],
    },
  ],
}

// A key cannot carry more than its creator holds, so offering the `admin` card
// to a `user` only ends in a refusal after the Review step.
test('a user is not offered a role above their own, with the reason', async () => {
  const api = mockApi({
    'GET /api-keys/permissions': () => jsonResponse(CEILING_CATALOGUE),
  })
  const view = renderPage('user')
  const name = view.container.querySelector<HTMLInputElement>('#name')
  fireEvent.change(name!, { target: { value: 'ci-key' } })
  fireEvent.click(view.getByRole('button', { name: 'Next: Permissions' }))
  await view.findByText('Standard access')

  const adminCard = view.getByText('Full access').closest('[aria-disabled]')
  expect(adminCard?.getAttribute('aria-disabled')).toBe('true')
  expect(adminCard?.textContent).toContain('Not available')
  expect(adminCard?.textContent).toContain('(user)')
  const userCard = view.getByText('Standard access').closest('.border-2')
  expect(userCard?.getAttribute('aria-disabled')).toBeNull()

  // Clicking the unavailable card selects nothing.
  fireEvent.click(adminCard!)
  expect(
    view.getByRole('button', { name: 'Next: Review' }).hasAttribute('disabled')
  ).toBe(true)

  // The custom list omits permissions the user does not hold either.
  expect(view.getByLabelText('Select all Domains permissions')).toBeTruthy()
  expect(view.queryByLabelText('Select all System permissions')).toBeNull()
  api.restore()
})

test('a role preselected from the link is dropped when the user cannot grant it', async () => {
  const api = mockApi({
    'GET /api-keys/permissions': () => jsonResponse(CEILING_CATALOGUE),
  })
  const view = renderPage('user', '/settings/keys/new?role=admin&name=ci-key')
  fireEvent.click(
    await view.findByRole('button', { name: 'Next: Permissions' })
  )
  await view.findByText('Standard access')
  expect(
    view.getByRole('button', { name: 'Next: Review' }).hasAttribute('disabled')
  ).toBe(true)
  api.restore()
})

test('an admin is offered every role', async () => {
  const api = mockApi({
    'GET /api-keys/permissions': () => jsonResponse(CEILING_CATALOGUE),
  })
  const view = renderPage('admin')
  const name = view.container.querySelector<HTMLInputElement>('#name')
  fireEvent.change(name!, { target: { value: 'ci-key' } })
  fireEvent.click(view.getByRole('button', { name: 'Next: Permissions' }))
  await view.findByText('Full access')
  expect(view.container.querySelector('[aria-disabled]')).toBeNull()
  api.restore()
})

// Until the role catalogue arrives the user's own permissions are unknown, so a
// role preselected from the link cannot be judged yet. Proceeding in that
// window would carry an ungrantable role to the Review step.
test('a role preselected from the link cannot be carried past a catalogue that has not loaded', async () => {
  let release: () => void = () => {}
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  const api = mockApi({
    'GET /api-keys/permissions': async () => {
      await gate
      return jsonResponse(CEILING_CATALOGUE)
    },
  })
  const view = renderPage('user', '/settings/keys/new?role=admin&name=ci-key')
  fireEvent.click(
    await view.findByRole('button', { name: 'Next: Permissions' })
  )
  const next = view.getByRole('button', { name: 'Next: Review' })
  expect(next.hasAttribute('disabled')).toBe(true)

  release()
  await view.findByText('Standard access')
  expect(
    view.getByRole('button', { name: 'Next: Review' }).hasAttribute('disabled')
  ).toBe(true)
  api.restore()
})
