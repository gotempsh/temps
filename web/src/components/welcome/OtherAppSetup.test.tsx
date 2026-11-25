// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import type { UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { OtherAppSetup } from './OtherAppSetup'

installBrowserWindow()
afterEach(cleanup)

function renderAsAdmin() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: { id: 1, role: 'admin' } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <OtherAppSetup />
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('the settings appear once the server is on and a key exists, with the key in them', async () => {
  let sent: { name?: string; role_type?: string } = {}
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: true } }),
    'POST /api-keys': async (request) => {
      sent = await request.json()
      return jsonResponse({
        id: 9,
        name: sent.name,
        key_prefix: 'tk_abc12',
        api_key: 'tk_abc12_secret',
        role_type: 'user',
        created_at: '2026-11-25T10:00:00Z',
      })
    },
  })
  const view = renderAsAdmin()
  await view.findByText('First create a key, then copy the settings.')
  expect(view.queryByLabelText('Cursor settings')).toBeNull()

  fireEvent.click(view.getByRole('combobox', { name: 'Which app?' }))
  fireEvent.click(await view.findByRole('option', { name: /VS Code/ }))
  fireEvent.click(view.getByRole('button', { name: 'Create key' }))

  const settings = await view.findByLabelText('VS Code settings')
  expect(sent.name).toMatch(/^mcp-vscode-/)
  expect(sent.role_type).toBe('user')
  expect(JSON.parse(settings.textContent ?? '')).toEqual({
    servers: {
      temps: {
        type: 'http',
        url: `${window.location.origin}/mcp?write=1`,
        headers: { Authorization: 'Bearer tk_abc12_secret' },
      },
    },
  })
  expect(
    view.getByText(/bunx @temps-sdk\/cli mcp add vscode --url .* --write -y/)
  ).toBeTruthy()
  api.restore()
})

test('with the MCP server off, nothing to copy yet', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: false } }),
  })
  const view = renderAsAdmin()
  await view.findByText(
    'First turn on the MCP server and create a key, then copy the settings.'
  )
  api.restore()
})

test('a second command lets the app deploy folders from this computer', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: false } }),
  })
  const view = renderAsAdmin()
  await view.findByText(/deploy project folders from this computer too/)
  expect(
    view.getByText(
      `bunx @temps-sdk/cli mcp add cursor --url ${window.location.origin} --write --deploy-folder ~/code -y`
    )
  ).toBeTruthy()
  api.restore()
})
