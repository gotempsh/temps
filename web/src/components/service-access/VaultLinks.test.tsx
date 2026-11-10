// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  render,
  within,
  fireEvent,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { mockApi, jsonResponse } from '@/test/fetch-mock'
import { VaultLinks } from './VaultLinks'
installBrowserWindow()
afterEach(cleanup)

test('unlinking the final row on page two returns to the remaining consumers', async () => {
  let removed = false
  const pages: number[] = []
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': (request) => {
      const page = Number(new URL(request.url).searchParams.get('page'))
      pages.push(page)
      const ids =
        page === 1
          ? Array.from({ length: 20 }, (_, i) => i + 1)
          : removed
            ? []
            : [21]
      return jsonResponse({
        items: ids.map((id) => ({
          id: `link_${id}`,
          vault_id: 'vault_fixture',
          project_id: id,
          sandbox_id: null,
          consumer_name: `Project ${id}`,
          created_at: '2026-01-01T00:00:00Z',
        })),
        total: removed ? 20 : 21,
        page,
        page_size: 20,
      })
    },
    'DELETE /access-vaults/vault_fixture/links/link_21': () => {
      removed = true
      return new Response(null, { status: 204 })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultLinks vaultId="vault_fixture" />
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Project 1')).toBeTruthy())
    fireEvent.click(ui.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(ui.getByText('Project 21')).toBeTruthy())
    fireEvent.click(ui.getByRole('button', { name: 'Unlink' }))
    fireEvent.click(
      within(ui.getByRole('alertdialog')).getByRole('button', {
        name: 'Unlink and revoke access',
      })
    )
    await waitFor(() => {
      expect(ui.queryByRole('alertdialog')).toBeNull()
      expect(ui.getByText('Project 1')).toBeTruthy()
      expect(ui.getAllByRole('button', { name: 'Unlink' })).toHaveLength(20)
    })
    expect(removed).toBe(true)
    expect(pages).toContain(2)
    expect(ui.queryByText(/No consumers linked/)).toBeNull()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('unlink requires a revocation confirmation and preserves failures', async () => {
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': () =>
      jsonResponse({
        items: [
          {
            id: 'link_fixture',
            vault_id: 'vault_fixture',
            project_id: 7,
            sandbox_id: null,
            consumer_name: 'Fixture project',
            created_at: '2026-01-01T00:00:00Z',
          },
        ],
        total: 1,
        page: 1,
        page_size: 20,
      }),
    'DELETE /access-vaults/vault_fixture/links/link_fixture': () =>
      jsonResponse({ detail: 'Storage temporarily unavailable' }, 503),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultLinks vaultId="vault_fixture" />
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Fixture project')).toBeTruthy())
    fireEvent.click(ui.getByRole('button', { name: 'Unlink' }))
    const dialog = within(ui.getByRole('alertdialog'))
    expect(dialog.getByText(/Live sessions and grants/)).toBeTruthy()
    expect(
      api.called('DELETE', '/access-vaults/vault_fixture/links/link_fixture')
    ).toBe(false)
    fireEvent.click(
      dialog.getByRole('button', { name: 'Unlink and revoke access' })
    )
    await waitFor(() =>
      expect(dialog.getByText('Storage temporarily unavailable')).toBeTruthy()
    )
    expect(
      within(ui.getByRole('table', { hidden: true })).getByText(
        'Fixture project'
      )
    ).toBeTruthy()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('loading displays skeleton rows before consumer data arrives', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('aria-label="Loading links"')
    expect(html).toContain('animate-pulse')
  } finally {
    client.clear()
  }
})

for (const kind of ['project', 'sandbox'] as const) {
  test(`link form validates a ${kind} selection and sends the typed consumer`, async () => {
    const bodies: unknown[] = []
    const api = mockApi({
      'GET /access-vaults/vault_fixture/links': () =>
        jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
      'GET /projects': () =>
        jsonResponse({
          projects: [
            { id: 7, name: 'Fixture project', source_type: 'external' },
          ],
          total: 1,
        }),
      'GET /v1/sandboxes': () =>
        jsonResponse({
          sandboxes: [{ id: 'sbx_fixture', name: 'Fixture sandbox' }],
          pagination: { count: 1 },
        }),
      'POST /access-vaults/vault_fixture/links': async (request) => {
        bodies.push(await request.json())
        return jsonResponse(
          { id: 'link_fixture', vault_id: 'vault_fixture' },
          201
        )
      },
    })
    const client = new QueryClient({
      defaultOptions: {
        queries: { retry: false },
        mutations: { retry: false },
      },
    })
    try {
      render(
        <QueryClientProvider client={client}>
          <VaultLinks vaultId="vault_fixture" />
        </QueryClientProvider>
      )
      const ui = within(document.body)
      fireEvent.click(
        ui.getByRole('button', { name: 'Link project or sandbox' })
      )
      const dialog = within(ui.getByRole('dialog'))
      if (kind === 'sandbox')
        fireEvent.click(dialog.getByRole('button', { name: 'Sandbox' }))
      fireEvent.click(dialog.getByRole('button', { name: 'Link consumer' }))
      await waitFor(() =>
        expect(dialog.getByRole('alert').textContent).toBe(`Select a ${kind}.`)
      )
      expect(bodies).toHaveLength(0)
      fireEvent.click(
        await dialog.findByRole('button', { name: `Fixture ${kind}` })
      )
      fireEvent.click(dialog.getByRole('button', { name: 'Link consumer' }))
      await waitFor(() => expect(ui.queryByRole('dialog')).toBeNull())
      expect(bodies).toEqual([
        kind === 'project' ? { project_id: 7 } : { sandbox_id: 'sbx_fixture' },
      ])
    } finally {
      cleanup()
      client.clear()
      api.restore()
    }
  })
}

test('empty links keep the status and linking action in the heading row', async () => {
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultLinks vaultId="vault_fixture" />
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() =>
      expect(ui.getByText('No consumers linked')).toBeTruthy()
    )
    const row = ui.getByRole('heading', { name: 'Linked to' }).parentElement!
    expect(within(row).getByText('No consumers linked')).toBeTruthy()
    fireEvent.click(
      within(row).getByRole('button', { name: 'Link project or sandbox' })
    )
    await waitFor(() =>
      expect(ui.getByRole('dialog', { name: 'Link a consumer' })).toBeTruthy()
    )
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
