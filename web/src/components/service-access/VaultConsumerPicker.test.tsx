// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test } from 'bun:test'
import {
  cleanup,
  render,
  fireEvent,
  waitFor,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { VaultConsumerPicker } from './VaultConsumerPicker'

installBrowserWindow()

test('project consumers show the shared project icon and preserve selection', async () => {
  const api = mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [
          {
            id: 7,
            name: 'Fixture project',
            source_type: 'external',
            service_template_image_url: 'https://example.com/project.svg',
          },
        ],
        total: 1,
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const selected: (number | string)[] = []
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultConsumerPicker
          kind="project"
          onSelect={(id) => selected.push(id)}
        />
      </QueryClientProvider>
    )
    const ui = within(document.body)
    const button = await ui.findByRole('button', { name: 'Fixture project' })
    await waitFor(() =>
      expect(button.querySelector('img')?.getAttribute('src')).toBe(
        'https://example.com/project.svg'
      )
    )
    fireEvent.click(button)
    expect(selected).toEqual([7])
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
          <VaultConsumerPicker kind="project" onSelect={() => {}} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('aria-label="Loading consumers"')
    expect(html).toContain('animate-pulse')
  } finally {
    client.clear()
  }
})

for (const kind of ['project', 'sandbox'] as const) {
  test(`${kind} list uses the compact empty state with a relevant destination`, async () => {
    const api = mockApi({
      'GET /projects': () => jsonResponse({ projects: [], total: 0 }),
      'GET /v1/sandboxes': () =>
        jsonResponse({ sandboxes: [], pagination: { count: 0 } }),
    })
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    try {
      render(
        <QueryClientProvider client={client}>
          <MemoryRouter>
            <VaultConsumerPicker kind={kind} onSelect={() => {}} />
          </MemoryRouter>
        </QueryClientProvider>
      )
      const ui = within(document.body)
      const heading = await ui.findByRole('heading', {
        name: `No ${kind === 'project' ? 'projects' : 'sandboxes'} yet`,
      })
      expect(heading.closest('.min-h-60')).toBeTruthy()
      expect(
        ui
          .getByRole('link', {
            name: `View ${kind === 'project' ? 'projects' : 'sandboxes'}`,
          })
          .getAttribute('href')
      ).toBe(kind === 'project' ? '/projects' : '/sandboxes')
      expect(ui.queryByText('Retry')).toBeNull()
    } finally {
      cleanup()
      client.clear()
      api.restore()
    }
  })
}

test('a failed membership check pauses linking and retries instead of showing every consumer as unlinked', async () => {
  let membershipCalls = 0
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': () => {
      membershipCalls += 1
      if (membershipCalls === 1) {
        return jsonResponse({ detail: 'membership lookup failed' }, 500)
      }
      return jsonResponse({
        items: [
          {
            id: 'link_7',
            vault_id: 'vault_fixture',
            project_id: 7,
            sandbox_id: null,
            consumer_name: 'Already linked',
            created_at: '2026-01-01T00:00:00Z',
          },
        ],
        total: 1,
        page: 1,
        page_size: 100,
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
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <VaultConsumerPicker
            kind="project"
            linkAction
            vaultId="vault_fixture"
            onSelect={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await ui.findByText('Could not check which projects are already linked')
    for (const name of ['Link Already linked', 'Link Not yet linked']) {
      const button = ui.getByRole('button', { name }) as HTMLButtonElement
      expect(button.disabled).toBe(true)
    }
    fireEvent.click(ui.getByRole('button', { name: 'Retry' }))
    const linked = (await ui.findByRole('button', {
      name: 'Linked Already linked',
    })) as HTMLButtonElement
    expect(linked.disabled).toBe(true)
    const link = ui.getByRole('button', {
      name: 'Link Not yet linked',
    }) as HTMLButtonElement
    expect(link.disabled).toBe(false)
    expect(
      ui.queryByText('Could not check which projects are already linked')
    ).toBeNull()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
