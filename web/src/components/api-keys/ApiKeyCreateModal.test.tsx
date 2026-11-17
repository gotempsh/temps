// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ApiKeyCreateModal } from './ApiKeyCreateModal'
import type { CreateApiKeyRequest } from '@/api/client'
installBrowserWindow()
afterEach(cleanup)

test('project-scoped keys submit the selected resource boundary', async () => {
  let attempts = 0
  const api = mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [{ id: 7, name: 'App', slug: 'app' }],
        total: 1,
        page: 1,
        per_page: 100,
      }),
    'GET /projects/7/environments': () =>
      ++attempts === 1
        ? jsonResponse({ title: 'Unavailable' }, 500)
        : jsonResponse([{ id: 9, name: 'Preview' }]),
    'GET /api-keys/permissions': () =>
      jsonResponse({
        roles: [{ name: 'reader', permissions: ['projects:read'] }],
        permissions: [],
      }),
  })
  let submitted: CreateApiKeyRequest | undefined
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <ApiKeyCreateModal
        open
        onOpenChange={() => {}}
        onSubmit={(data) => {
          submitted = data
        }}
        isPending={false}
        newKeySecret={null}
        onKeySecretDismiss={() => {}}
      />
    </QueryClientProvider>
  )
  try {
    fireEvent.change(view.getByLabelText('Name'), {
      target: { value: 'Scoped key' },
    })
    await waitFor(() => expect(view.getAllByRole('combobox').length).toBe(2))
    const selects = view.getAllByRole('combobox')
    fireEvent.click(selects[selects.length - 1]!)
    fireEvent.click(await view.findByRole('option', { name: /reader/ }))
    fireEvent.click(
      await view.findByRole('combobox', { name: 'API key project scope' })
    )
    fireEvent.click(await view.findByText('App'))
    expect(await view.findByText('Environment scope')).toBeTruthy()
    fireEvent.click(await view.findByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(attempts).toBe(2))
    expect((view.getByLabelText('Name') as HTMLInputElement).value).toBe(
      'Scoped key'
    )
    expect(
      view.getByRole('combobox', { name: 'API key project scope' }).textContent
    ).toContain('App')
    const retainedSelects = view.getAllByRole('combobox')
    expect(retainedSelects[retainedSelects.length - 1]?.textContent).toContain(
      'reader'
    )
    fireEvent.click(view.getByRole('combobox', { name: 'Environment scope' }))
    fireEvent.click(await view.findByRole('option', { name: 'Preview' }))
    fireEvent.submit(
      view.getByRole('button', { name: 'Create' }).closest('form')!
    )
    await waitFor(() => expect(submitted?.project_id).toBe(7))
    expect(submitted?.environment_id).toBe(9)
    expect(submitted?.role_type).toBe('reader')
    fireEvent.click(
      view.getByRole('combobox', { name: 'API key project scope' })
    )
    fireEvent.click(await view.findByText('Account-wide access'))
    expect(view.queryByText('Environment scope')).toBeNull()
    fireEvent.submit(
      view.getByRole('button', { name: 'Create' }).closest('form')!
    )
    await waitFor(() => expect(submitted?.project_id).toBeNull())
    expect(submitted?.environment_id).toBeNull()
  } finally {
    api.restore()
  }
})
