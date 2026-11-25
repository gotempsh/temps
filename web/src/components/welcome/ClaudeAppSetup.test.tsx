// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import type { UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { ClaudeAppSetup } from './ClaudeAppSetup'

installBrowserWindow()
afterEach(cleanup)

function renderAs(role: string, onUseCodingAgent?: () => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: { id: 1, role } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <ClaudeAppSetup onUseCodingAgent={onUseCodingAgent} />
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('an admin sees the MCP server is off and can turn it on', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: false } }),
  })
  const view = renderAs('admin')
  await view.findByText('It is off, so nothing can connect to it yet.')
  expect(view.getByRole('button', { name: 'Turn on' })).toBeTruthy()
  api.restore()
})

test('anyone else is told an admin turns it on, without reading settings', async () => {
  const api = mockApi({})
  const view = renderAs('user')
  await view.findByText('An admin turns it on under Settings, MCP server.')
  expect(api.calls.some((call) => call.path === '/settings')).toBe(false)
  api.restore()
})

test('the key can make changes by default, is an MCP client key, and is shown once', async () => {
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
        role_type: 'reader',
        created_at: '2026-11-25T10:00:00Z',
      })
    },
  })
  const view = renderAs('admin')
  await view.findByText('The MCP server is on.')

  fireEvent.click(view.getByRole('button', { name: 'Create key' }))

  await view.findByText('tk_abc12_secret')
  // Writes are allowed by the key's role; each still waits for approval.
  expect(sent.role_type).toBe('user')
  expect(sent.name).toMatch(
    /^mcp-claude-app-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z$/
  )
  await waitFor(() =>
    expect(view.queryByRole('button', { name: 'Create key' })).toBeNull()
  )
  expect(view.getByRole('button', { name: 'Download temps.mcpb' })).toBeTruthy()
  api.restore()
})

test('it says how to install the file and what to enter', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: true } }),
  })
  const view = renderAs('admin')
  const steps = view.getByRole('list', { name: 'Install the extension' })
  const text = steps.textContent ?? ''
  expect(text).toContain('Settings, then Extensions')
  expect(text).toContain('Click Install')
  expect(text).toContain(window.location.origin)
  expect(text).toContain('paste the key from step 2')
  expect(text).toContain('Folders Claude may deploy')
  await view.findByText(/What’s running on my Temps server/)
  api.restore()
})

test('the download stays locked until the MCP server is on and a key exists', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: false } }),
    'POST /api-keys': () =>
      jsonResponse({
        id: 9,
        name: 'mcp-claude-app-2026-11-25T10-00-00Z',
        key_prefix: 'tk_abc12',
        api_key: 'tk_abc12_secret',
        role_type: 'reader',
        created_at: '2026-11-25T10:00:00Z',
      }),
  })
  const view = renderAs('admin')
  await view.findByText(
    'First turn on the MCP server and create a key, then download the extension.'
  )
  const download = view.getByRole('button', { name: 'Download temps.mcpb' })
  expect(download.getAttribute('aria-disabled')).toBe('true')

  fireEvent.click(view.getByRole('button', { name: 'Create key' }))
  await view.findByText(
    'First turn on the MCP server, then download the extension.'
  )
  expect(download.getAttribute('aria-disabled')).toBe('true')
  api.restore()
})

test('a read-only key is a choice away', async () => {
  let sent: { role_type?: string } = {}
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: true } }),
    'POST /api-keys': async (request) => {
      sent = await request.json()
      return jsonResponse({
        id: 9,
        name: 'mcp-claude-app-2026-11-25T10-00-00Z',
        key_prefix: 'tk_abc12',
        api_key: 'tk_abc12_secret',
        role_type: 'reader',
        created_at: '2026-11-25T10:00:00Z',
      })
    },
  })
  const view = renderAs('admin')
  await view.findByText('The MCP server is on.')
  fireEvent.click(view.getByRole('radio', { name: /Read only/ }))
  fireEvent.click(view.getByRole('button', { name: 'Create key' }))
  await view.findByText('tk_abc12_secret')
  expect(sent.role_type).toBe('reader')
  api.restore()
})

test('Cowork and Code sessions are pointed at the coding-agent prompt', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: true } }),
  })
  let switched = 0
  const view = renderAs('admin', () => switched++)
  await view.findByText(/Cowork and Code sessions don’t run extensions/)
  expect(view.getByText(/deploys with temps drop/)).toBeTruthy()
  fireEvent.click(
    view.getByRole('button', { name: 'Show the coding-agent prompt' })
  )
  expect(switched).toBe(1)
  api.restore()
})
