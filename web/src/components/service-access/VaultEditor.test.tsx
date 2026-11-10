// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { VaultEditor } from './VaultEditor'
import type { AccessVaultResponse } from '@/api/client'
installBrowserWindow()
afterEach(cleanup)
const vault: AccessVaultResponse = {
  id: 'vault_test',
  project_id: 7,
  name: 'Development',
  service_ids: ['svc_existing'],
  version: 3,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
test('vault edits send the selected version, keep existing members and preserve a rejected draft', async () => {
  let submitted: unknown
  const api = mockApi({
    'GET /projects/7/access-services': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
    'PUT /projects/7/access-vaults/vault_test': async (request) => {
      submitted = await request.json()
      return jsonResponse(
        { detail: 'Vault changed; reload before editing' },
        409
      )
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultEditor vault={vault} open canWrite onClose={() => {}} />
      </QueryClientProvider>
    )
    await waitFor(() =>
      expect(
        (
          within(document.body).getByRole('button', {
            name: 'Save vault',
          }) as HTMLButtonElement
        ).disabled
      ).toBe(false)
    )
    await act(async () =>
      fireEvent.change(within(document.body).getByLabelText('Vault name'), {
        target: { value: 'Staging' },
      })
    )
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Save vault' })
      )
    )
    await waitFor(() =>
      expect(
        within(document.body).getByText('Could not save vault')
      ).toBeTruthy()
    )
    expect(submitted).toEqual({
      name: 'Staging',
      service_ids: ['svc_existing'],
      expected_version: 3,
    })
    expect(
      (within(document.body).getByLabelText('Vault name') as HTMLInputElement)
        .value
    ).toBe('Staging')
  } finally {
    client.clear()
    api.restore()
  }
})

test('vault fields stay disabled until a pending save completes', async () => {
  let finish!: (response: Response) => void
  const pending = new Promise<Response>((resolve) => {
    finish = resolve
  })
  let closed = false
  const api = mockApi({
    'GET /projects/7/access-services': () =>
      jsonResponse({
        items: [
          {
            id: 'svc_existing',
            name: 'Example API',
            policy: { origin: 'https://api.example.com' },
          },
        ],
        total: 1,
        page: 1,
        page_size: 20,
      }),
    'PUT /projects/7/access-vaults/vault_test': () => pending,
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultEditor
          vault={vault}
          open
          canWrite
          onClose={() => {
            closed = true
          }}
        />
      </QueryClientProvider>
    )
    const name = within(document.body).getByLabelText(
      'Vault name'
    ) as HTMLInputElement
    const checkbox = (await within(document.body).findByRole(
      'checkbox'
    )) as HTMLButtonElement
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Save vault' })
      )
    )
    await waitFor(() => expect(name.disabled).toBe(true))
    expect(checkbox.disabled).toBe(true)
    expect(closed).toBe(false)
    await act(async () => {
      finish(jsonResponse({ detail: 'Temporary failure' }, 503))
    })
    await waitFor(() => expect(name.disabled).toBe(false))
    expect(checkbox.disabled).toBe(false)
    expect(closed).toBe(false)
    expect(name.value).toBe('Development')
  } finally {
    finish(jsonResponse(vault))
    client.clear()
    api.restore()
  }
})

test('a conflict preserves the draft until the user reviews and adopts the latest version', async () => {
  let saved = {
    ...vault,
    name: 'Updated by another editor',
    version: 4,
    service_ids: [] as string[],
  }
  const versions: number[] = []
  let failRead = true
  const api = mockApi({
    'GET /projects/7/access-services': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
    'GET /projects/7/access-vaults/vault_test': () =>
      failRead
        ? jsonResponse({ detail: 'Try again' }, 503)
        : jsonResponse({ vault: saved, services: [] }),
    'PUT /projects/7/access-vaults/vault_test': async (request) => {
      const body = (await request.json()) as {
        name: string
        service_ids: string[]
        expected_version: number
      }
      versions.push(body.expected_version)
      if (body.expected_version !== saved.version)
        return jsonResponse(
          { detail: 'Vault changed; reload before editing' },
          409
        )
      saved = {
        ...saved,
        name: body.name,
        service_ids: body.service_ids,
        version: saved.version + 1,
      }
      return jsonResponse(saved)
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultEditor vault={vault} open canWrite onClose={() => {}} />
      </QueryClientProvider>
    )
    const name = within(document.body).getByLabelText(
      'Vault name'
    ) as HTMLInputElement
    const click = async (label: string) => {
      await act(async () =>
        fireEvent.click(
          within(document.body).getByRole('button', { name: label })
        )
      )
    }
    await waitFor(() =>
      expect(
        (
          within(document.body).getByRole('button', {
            name: 'Save vault',
          }) as HTMLButtonElement
        ).disabled
      ).toBe(false)
    )
    await act(async () =>
      fireEvent.change(name, { target: { value: 'My preserved draft' } })
    )
    await click('Save vault')
    await waitFor(() =>
      expect(
        within(document.body).getByText('Could not save vault')
      ).toBeTruthy()
    )
    await click('Review latest version')
    await waitFor(() =>
      expect(
        within(document.body).getByText('Could not load the latest vault')
      ).toBeTruthy()
    )
    expect(name.value).toBe('My preserved draft')
    failRead = false
    await click('Review latest version')
    await waitFor(() =>
      expect(
        within(document.body).getByText(/Updated by another editor · Version 4/)
      ).toBeTruthy()
    )
    expect(name.value).toBe('My preserved draft')
    await click('Discard draft and use latest')
    expect(name.value).toBe('Updated by another editor')
    expect(
      within(document.body).getByText('Services · 0 selected')
    ).toBeTruthy()
    await click('Save vault')
    await waitFor(() => expect(saved.version).toBe(5))
    expect(versions).toEqual([3, 4])
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
