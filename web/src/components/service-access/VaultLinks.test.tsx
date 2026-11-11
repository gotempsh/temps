// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import {
  Link,
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from 'react-router'
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

function UrlProbe() {
  const location = useLocation()
  const navigate = useNavigate()
  return (
    <>
      <output aria-label="URL">
        {location.pathname}
        {location.search}
      </output>
      <button onClick={() => navigate(-1)}>Back</button>
    </>
  )
}

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
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture',
          ]}
        >
          <UrlProbe />
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Project 1')).toBeTruthy())
    fireEvent.click(ui.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(ui.getByText('Project 21')).toBeTruthy())
    expect(ui.getByLabelText('URL').textContent).toBe(
      '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture&links_page=2'
    )
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
    expect(ui.getByLabelText('URL').textContent).toBe(
      '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture'
    )
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
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture',
          ]}
        >
          <UrlProbe />
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
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
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture',
          ]}
        >
          <UrlProbe />
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
  test(`row action links a ${kind} and sends the typed consumer`, async () => {
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
          <MemoryRouter
            initialEntries={[
              '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture',
            ]}
          >
            <UrlProbe />
            <VaultLinks vaultId="vault_fixture" />
          </MemoryRouter>
        </QueryClientProvider>
      )
      const ui = within(document.body)
      const dialog = within(ui.getByRole('region', { name: 'Link a consumer' }))
      expect(
        await dialog.findByRole('button', { name: 'Link Fixture project' })
      ).toBeTruthy()
      expect(
        await dialog.findByRole('button', { name: 'Link Fixture sandbox' })
      ).toBeTruthy()
      expect(bodies).toHaveLength(0)
      fireEvent.click(
        await dialog.findByRole('button', { name: `Link Fixture ${kind}` })
      )
      await waitFor(() => expect(bodies).toHaveLength(1))
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

test('empty links show both consumer lists without opening a dialog', async () => {
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
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture',
          ]}
        >
          <UrlProbe />
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    expect(ui.queryByText('No consumers linked')).toBeNull()
    expect(ui.getByRole('region', { name: 'Projects' })).toBeTruthy()
    expect(ui.getByRole('region', { name: 'Sandboxes' })).toBeTruthy()
    expect(ui.getByRole('region', { name: 'Link a consumer' })).toBeTruthy()
    expect(ui.queryByRole('dialog')).toBeNull()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('linked-consumer URLs restore pagination and browser history without changing vault context', async () => {
  const pages: number[] = []
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': (request) => {
      const page = Number(new URL(request.url).searchParams.get('page'))
      pages.push(page)
      return jsonResponse({
        items: [
          {
            id: `link_${page}`,
            vault_id: 'vault_fixture',
            project_id: page,
            sandbox_id: null,
            consumer_name: `Consumer page ${page}`,
            created_at: '2026-01-01T00:00:00Z',
          },
        ],
        total: 60,
        page,
        page_size: 20,
      })
    },
  })
  const url =
    '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture&page=4&links_page=2'
  const clients: QueryClient[] = []
  const mount = () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    clients.push(client)
    return render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[url]}>
          <UrlProbe />
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
      </QueryClientProvider>
    )
  }
  try {
    const first = mount()
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Consumer page 2')).toBeTruthy())
    expect(pages[0]).toBe(2)
    fireEvent.click(ui.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(ui.getByText('Consumer page 3')).toBeTruthy())
    expect(ui.getByLabelText('URL').textContent).toBe(
      url.replace('links_page=2', 'links_page=3')
    )
    fireEvent.click(ui.getByRole('button', { name: 'Back' }))
    await waitFor(() => expect(ui.getByText('Consumer page 2')).toBeTruthy())
    expect(ui.getByLabelText('URL').textContent).toBe(url)
    first.unmount()
    mount()
    await waitFor(() => expect(ui.getByText('Consumer page 2')).toBeTruthy())
    expect(ui.getByLabelText('URL').textContent).toBe(url)
  } finally {
    cleanup()
    clients.forEach((client) => client.clear())
    api.restore()
  }
})

test.each(['invalid', '-1', '2.5', '99'])(
  'recovers a linked-consumer page of %s',
  async (value) => {
    const api = mockApi({
      'GET /access-vaults/vault_fixture/links': (request) => {
        const page = Number(new URL(request.url).searchParams.get('page'))
        return jsonResponse({
          items:
            page === 1
              ? [
                  {
                    id: 'link_fixture',
                    vault_id: 'vault_fixture',
                    project_id: 7,
                    sandbox_id: null,
                    consumer_name: 'Remaining consumer',
                    created_at: '2026-01-01T00:00:00Z',
                  },
                ]
              : [],
          total: 1,
          page,
          page_size: 20,
        })
      },
    })
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    try {
      render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={[
              `/vault/vaults/vault_fixture?tab=links&links_page=${value}`,
            ]}
          >
            <UrlProbe />
            <VaultLinks vaultId="vault_fixture" />
          </MemoryRouter>
        </QueryClientProvider>
      )
      const ui = within(document.body)
      await waitFor(() =>
        expect(ui.getByText('Remaining consumer')).toBeTruthy()
      )
      if (value === '99')
        expect(ui.getByLabelText('URL').textContent).toBe(
          '/vault/vaults/vault_fixture?tab=links'
        )
    } finally {
      cleanup()
      client.clear()
      api.restore()
    }
  }
)

test.each(['link', 'back'])(
  'pending unlink cannot navigate after leaving via %s',
  async (navigation) => {
    let finishDelete: (response: Response) => void = () => {}
    const pendingDelete = new Promise<Response>((resolve) => {
      finishDelete = resolve
    })
    const api = mockApi({
      'GET /access-vaults/vault_fixture/links': () =>
        jsonResponse({
          items: [
            {
              id: 'link_fixture',
              vault_id: 'vault_fixture',
              project_id: 7,
              sandbox_id: null,
              consumer_name: 'Fixture consumer',
              created_at: '2026-01-01T00:00:00Z',
            },
          ],
          total: 21,
          page: 2,
          page_size: 20,
        }),
      'DELETE /access-vaults/vault_fixture/links/link_fixture': () =>
        pendingDelete,
    })
    const client = new QueryClient({
      defaultOptions: {
        queries: { retry: false },
        mutations: { retry: false },
      },
    })
    const sessionKey = [{ _id: 'listAccessSessionsGeneral' }]
    client.setQueryData(sessionKey, [])
    try {
      render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={[
              '/elsewhere',
              '/vault/vaults/vault_fixture?tab=links&links_page=2',
            ]}
          >
            <UrlProbe />
            <Link to="/elsewhere">Leave vault</Link>
            <Routes>
              <Route
                path="/vault/vaults/:vaultId"
                element={<VaultLinks vaultId="vault_fixture" />}
              />
              <Route path="/elsewhere" element={<h1>Another task</h1>} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>
      )
      const ui = within(document.body)
      fireEvent.click(await ui.findByRole('button', { name: 'Unlink' }))
      fireEvent.click(
        within(ui.getByRole('alertdialog')).getByRole('button', {
          name: 'Unlink and revoke access',
        })
      )
      await waitFor(() =>
        expect(
          api.called(
            'DELETE',
            '/access-vaults/vault_fixture/links/link_fixture'
          )
        ).toBe(true)
      )
      fireEvent.click(
        navigation === 'back'
          ? ui.getByRole('button', { name: 'Back', hidden: true })
          : ui.getByRole('link', { name: 'Leave vault', hidden: true })
      )
      await waitFor(() =>
        expect(ui.getByRole('heading', { name: 'Another task' })).toBeTruthy()
      )
      expect(ui.queryByRole('table')).toBeNull()
      finishDelete(new Response(null, { status: 204 }))
      await waitFor(() =>
        expect(client.getMutationCache().getAll()[0]?.state.status).toBe(
          'success'
        )
      )
      expect(ui.getByLabelText('URL').textContent).toBe('/elsewhere')
      expect(client.getQueryState(sessionKey)?.isInvalidated).toBe(true)
      expect(
        client
          .getQueryCache()
          .findAll({ queryKey: [{ _id: 'listVaultLinks' }] })
          .every((query) => query.state.isInvalidated)
      ).toBe(true)
    } finally {
      finishDelete(new Response(null, { status: 204 }))
      cleanup()
      client.clear()
      api.restore()
    }
  }
)

test('inline consumer lists restore independent pages and preserve vault context', async () => {
  const requested: string[] = []
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
    'GET /projects': (request) => {
      const page = new URL(request.url).searchParams.get('page')
      requested.push(`project:${page}`)
      return jsonResponse({
        projects: [
          { id: 7, name: `Project page ${page}`, source_type: 'external' },
        ],
        total: 60,
      })
    },
    'GET /v1/sandboxes': (request) => {
      const page = new URL(request.url).searchParams.get('page')
      requested.push(`sandbox:${page}`)
      return jsonResponse({
        sandboxes: [{ id: 'sbx_fixture', name: `Sandbox page ${page}` }],
        pagination: { count: 60 },
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture&links_project_page=2&links_sandbox_page=3',
          ]}
        >
          <UrlProbe />
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    expect(
      await ui.findByRole('button', { name: 'Link Project page 2' })
    ).toBeTruthy()
    expect(
      await ui.findByRole('button', { name: 'Link Sandbox page 3' })
    ).toBeTruthy()
    expect(requested).toEqual(
      expect.arrayContaining(['project:2', 'sandbox:3'])
    )
    fireEvent.click(
      within(ui.getByRole('region', { name: 'Projects' })).getByRole('button', {
        name: 'Next page',
      })
    )
    expect(
      await ui.findByRole('button', { name: 'Link Project page 3' })
    ).toBeTruthy()
    expect(ui.getByLabelText('URL').textContent).toBe(
      '/vault/vaults/vault_fixture?tab=links&sandbox=sbx_fixture&links_project_page=3&links_sandbox_page=3'
    )
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('a linked consumer reads Linked even when its link is not on the visible links page', async () => {
  const filters: string[] = []
  const links = new Set<number>([7])
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': (request) => {
      const params = new URL(request.url).searchParams
      const wanted = params.get('project_ids')
      const item = (id: number) => ({
        id: `link_${id}`,
        vault_id: 'vault_fixture',
        project_id: id,
        sandbox_id: null,
        consumer_name: `Project ${id}`,
        created_at: '2026-01-01T00:00:00Z',
      })
      if (wanted !== null) {
        filters.push(wanted)
        const items = wanted
          .split(',')
          .map(Number)
          .filter((id) => links.has(id))
          .map(item)
        return jsonResponse({
          items,
          total: items.length,
          page: 1,
          page_size: 100,
        })
      }
      // The visible page of 20 never contains projects 7 or 8.
      return jsonResponse({
        items: Array.from({ length: 20 }, (_, i) => item(100 + i)),
        total: 22,
        page: 1,
        page_size: 20,
      })
    },
    'GET /projects': () =>
      jsonResponse({
        projects: [
          { id: 7, name: 'Already linked', source_type: 'external' },
          { id: 8, name: 'Not yet linked', source_type: 'external' },
        ],
        total: 2,
      }),
    'GET /v1/sandboxes': () =>
      jsonResponse({ sandboxes: [], pagination: { count: 0 } }),
    'POST /access-vaults/vault_fixture/links': async (request) => {
      const body = (await request.json()) as { project_id: number }
      links.add(body.project_id)
      return jsonResponse(
        { id: `link_${body.project_id}`, vault_id: 'vault_fixture' },
        201
      )
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={['/vault/vaults/vault_fixture?tab=links']}
        >
          <VaultLinks vaultId="vault_fixture" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    const linked = await ui.findByRole('button', {
      name: 'Linked Already linked',
    })
    expect((linked as HTMLButtonElement).disabled).toBe(true)
    expect(filters).toContain('7,8')
    const link = await ui.findByRole('button', { name: 'Link Not yet linked' })
    fireEvent.click(link)
    // Once the membership check refreshes, the new link reads Linked too.
    expect(
      await ui.findByRole('button', { name: 'Linked Not yet linked' })
    ).toBeTruthy()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
