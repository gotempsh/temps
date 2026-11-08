// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import { AuthContext } from '@/contexts/AuthContext-shared'
import type { UserResponse } from '@/api/client/types.gen'
import { PluginAiTools } from './PluginAiTools'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'success').mockRestore()
  spyOn(toast, 'error').mockRestore()
})
const settings = {
  actor_id: '00000000-0000-4000-8000-000000000001',
  installation_revision: 'binary-one',
  enabled: false,
  tools: [
    {
      name: 'plugin.reports.list',
      access: 'read',
      description: 'List reports',
    },
    {
      name: 'plugin.reports.create',
      access: 'write',
      description: 'Create a report',
    },
  ],
}
function mount(open = true, role = 'admin') {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: {
        retry: false,
        onError: (error, _variables, _result, context) =>
          announceMutationFailure(
            error as { title?: string; detail?: string },
            context
          ),
      },
    },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: { role } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <PluginAiTools name="reports" />
      </AuthContext.Provider>
    </QueryClientProvider>
  )
  if (open) fireEvent.click(view.getByRole('button', { name: 'AI chat tools' }))
  return view
}

test('keeps an always-visible compact entry point without opening optional settings', async () => {
  mockApi({ 'GET /x/plugins/reports/ai-tools': () => jsonResponse(settings) })
  const view = mount(false)
  expect(await view.findByText('AI access off · 2 tools')).toBeTruthy()
  expect(view.queryByRole('dialog')).toBeNull()
  expect(view.queryByRole('checkbox')).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'AI chat tools' }))
  expect(await view.findByRole('dialog')).toBeTruthy()
  expect(view.getByText('List reports')).toBeTruthy()
})

test('keeps the empty state discoverable before opening settings', async () => {
  mockApi({
    'GET /x/plugins/reports/ai-tools': () =>
      jsonResponse({ ...settings, tools: [] }),
  })
  const view = mount(false)
  expect(await view.findByText('No AI tools declared')).toBeTruthy()
  expect(view.getByRole('button', { name: 'AI chat tools' })).toBeTruthy()
})

test('lists read/write tools and saves consent with actor identity and success feedback', async () => {
  const success = spyOn(toast, 'success').mockImplementation(() => 'id')
  let submitted: unknown
  mockApi({
    'GET /x/plugins/reports/ai-tools': () => jsonResponse(settings),
    'PUT /x/plugins/reports/ai-tools': async (request) => {
      submitted = await request.json()
      return jsonResponse({ ...settings, enabled: true })
    },
  })
  const view = mount()
  expect(await view.findByText('List reports')).toBeTruthy()
  expect(view.getByText('write')).toBeTruthy()
  const toggle = view.getByRole('checkbox', {
    name: 'Allow AI chat to use these tools',
  })
  expect(toggle.getAttribute('aria-checked')).toBe('false')
  fireEvent.click(toggle)
  expect(submitted).toBeUndefined()
  fireEvent.click(view.getByRole('button', { name: 'Save' }))
  await waitFor(() => expect(success).toHaveBeenCalledTimes(1))
  expect(submitted).toEqual({
    actor_id: settings.actor_id,
    installation_revision: settings.installation_revision,
    enabled: true,
  })
  expect(success).toHaveBeenCalledTimes(1)
})

test('always renders the AI tools section and empty state', async () => {
  mockApi({
    'GET /x/plugins/reports/ai-tools': () =>
      jsonResponse({ ...settings, tools: [] }),
  })
  const view = mount()
  expect(
    await view.findByText('This plugin declares no AI tools.')
  ).toBeTruthy()
  expect(view.getByText('AI chat tools')).toBeTruthy()
  expect(view.getByRole('checkbox')).toBeTruthy()
})

test('failed toggle preserves consent and shows an error toast and retry guidance', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  mockApi({
    'GET /x/plugins/reports/ai-tools': () => jsonResponse(settings),
    'PUT /x/plugins/reports/ai-tools': () =>
      jsonResponse(
        { title: 'Conflict', detail: 'Plugin identity changed' },
        409
      ),
  })
  const view = mount()
  const toggle = await view.findByRole('checkbox')
  fireEvent.click(toggle)
  fireEvent.click(view.getByRole('button', { name: 'Save' }))
  expect(await view.findByRole('alert')).toBeTruthy()
  expect(toggle.getAttribute('aria-checked')).toBe('true')
  expect(error).toHaveBeenCalledTimes(1)
})

test('failed reads show a retry instead of a misleading empty state', async () => {
  mockApi({
    'GET /x/plugins/reports/ai-tools': () =>
      jsonResponse({ title: 'Unavailable' }, 503),
  })
  const view = mount()
  expect(await view.findByRole('alert')).toBeTruthy()
  expect(view.getByRole('button', { name: 'Retry' })).toBeTruthy()
  expect(view.queryByText('This plugin declares no AI tools.')).toBeNull()
})

test.each(['actor', 'binary'])(
  'reloads a replacement %s and requires a new consent choice after conflict',
  async (kind) => {
    const replacement = {
      ...settings,
      actor_id:
        kind === 'actor'
          ? '00000000-0000-4000-8000-000000000002'
          : settings.actor_id,
      installation_revision:
        kind === 'binary' ? 'binary-two' : settings.installation_revision,
    }
    let replaced = false
    const submitted: unknown[] = []
    mockApi({
      'GET /x/plugins/reports/ai-tools': () =>
        jsonResponse(replaced ? replacement : settings),
      'PUT /x/plugins/reports/ai-tools': async (request) => {
        submitted.push(await request.json())
        if (!replaced) {
          replaced = true
          return jsonResponse(
            { title: 'Conflict', detail: 'Plugin identity changed' },
            409
          )
        }
        return jsonResponse({ ...replacement, enabled: true })
      },
    })
    const view = mount()
    const toggle = await view.findByRole('checkbox')
    await waitFor(() =>
      expect(toggle.getAttribute('aria-checked')).toBe('false')
    )
    fireEvent.click(toggle)
    fireEvent.click(view.getByRole('button', { name: 'Save' }))
    fireEvent.click(
      await view.findByRole('button', { name: 'Reload settings' })
    )
    await waitFor(() =>
      expect(toggle.getAttribute('aria-checked')).toBe('false')
    )
    expect(submitted).toEqual([
      {
        actor_id: settings.actor_id,
        installation_revision: settings.installation_revision,
        enabled: true,
      },
    ])
    expect(
      view.getByRole('button', { name: 'Save' }).hasAttribute('disabled')
    ).toBe(true)
    fireEvent.click(toggle)
    fireEvent.click(view.getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(submitted).toEqual([
        {
          actor_id: settings.actor_id,
          installation_revision: settings.installation_revision,
          enabled: true,
        },
        {
          actor_id: replacement.actor_id,
          installation_revision: replacement.installation_revision,
          enabled: true,
        },
      ])
    )
  }
)

test('does not request or show admin consent settings for non-administrators', async () => {
  let requests = 0
  mockApi({
    'GET /x/plugins/reports/ai-tools': () => {
      requests++
      return jsonResponse(settings)
    },
  })
  const view = mount(false, 'user')
  await new Promise((resolve) => setTimeout(resolve, 10))
  expect(view.queryByRole('button', { name: 'AI chat tools' })).toBeNull()
  expect(requests).toBe(0)
})
