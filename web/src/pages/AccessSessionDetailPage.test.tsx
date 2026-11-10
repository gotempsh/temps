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
import { MemoryRouter, Route, Routes } from 'react-router'
import type { AccessSessionResponse } from '@/api/client'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import AccessSessionDetailPage from './AccessSessionDetailPage'

installBrowserWindow()
afterEach(cleanup)

test('session revocation requires named confirmation, holds pending, and lets a rejected request retry', async () => {
  let session: AccessSessionResponse = {
    id: 'session_example',
    project_id: 7,
    vault_id: 'vault_example',
    bundle_id: 'bundle_example',
    bundle_name: 'Read APIs',
    bundle_version: 1,
    consumer: { kind: 'sandbox', sandbox_id: 'sbx_example' },
    service_count: 1,
    status: 'issued',
    created_at: '2026-01-01T00:00:00Z',
    expires_at: '2100-01-01T00:00:00Z',
  }
  let calls = 0
  let finish: ((response: Response) => void) | undefined
  const api = mockApi({
    'GET /service-access/capability': () =>
      jsonResponse({ configured: true, can_manage_requests: true }),
    'GET /access-sessions/session_example': () =>
      jsonResponse({ session, grants: [] }),
    'POST /access-sessions/session_example/revoke': () => {
      calls += 1
      return new Promise<Response>((resolve) => {
        finish = resolve
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_example/sessions/session_example',
          ]}
        >
          <BreadcrumbProvider>
            <Routes>
              <Route
                path="/vault/vaults/:vaultId/sessions/:sessionId"
                element={<AccessSessionDetailPage />}
              />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    const open = async () => {
      const button = await within(view.container).findByRole('button', {
        name: 'Revoke session',
      })
      await waitFor(() =>
        expect((button as HTMLButtonElement).disabled).toBe(false)
      )
      await act(async () => {
        fireEvent.click(button)
      })
      return within(document.body).getByRole('alertdialog')
    }
    await waitFor(() =>
      expect(
        within(view.container)
          .getByRole('link', { name: 'All sessions' })
          .getAttribute('href')
      ).toBe('/vault/vaults/vault_example?tab=sessions&sandbox=sbx_example')
    )
    let dialog = await open()
    expect(dialog.textContent).toContain('Read APIs')
    expect(dialog.textContent).toContain('session_example')
    expect(dialog.textContent).toContain('sbx_example')
    expect(calls).toBe(0)
    await act(async () => {
      fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    })
    expect(calls).toBe(0)
    dialog = await open()
    const confirm = within(dialog).getByRole('button', {
      name: 'Revoke session',
    }) as HTMLButtonElement
    expect(confirm.disabled).toBe(true)
    await act(async () => {
      fireEvent.change(within(dialog).getByRole('textbox'), {
        target: { value: 'Read APIs' },
      })
    })
    await act(async () => {
      fireEvent.click(confirm)
    })
    await waitFor(() => expect(calls).toBe(1))
    expect(
      (
        within(dialog).getByRole('button', {
          name: 'Cancel',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    expect(
      (
        within(dialog).getByRole('button', {
          name: 'Working…',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    await act(async () => {
      finish?.(
        jsonResponse({ detail: 'Revocation temporarily unavailable' }, 503)
      )
    })
    await waitFor(() =>
      expect(
        within(dialog).getByText('Revocation temporarily unavailable')
      ).toBeTruthy()
    )
    expect(within(document.body).getByRole('alertdialog')).toBeTruthy()
    await act(async () => {
      fireEvent.click(
        within(dialog).getByRole('button', { name: 'Revoke session' })
      )
    })
    await waitFor(() => expect(calls).toBe(2))
    session = {
      ...session,
      status: 'revoked',
      revoked_at: new Date().toISOString(),
    }
    await act(async () => {
      finish?.(jsonResponse(session))
    })
    await waitFor(() =>
      expect(within(document.body).queryByRole('alertdialog')).toBeNull()
    )
    await waitFor(() =>
      expect(
        within(view.container).getByText('Revocation requested')
      ).toBeTruthy()
    )
    view.unmount()
  } finally {
    client.clear()
    api.restore()
  }
})
