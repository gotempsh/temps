// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import { PluginAiTools } from './PluginAiTools'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'success').mockRestore()
  spyOn(toast, 'error').mockRestore()
})
const settings = {
  actor_id: '00000000-0000-4000-8000-000000000001',
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
function mount() {
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
  return render(
    <QueryClientProvider client={client}>
      <PluginAiTools name="reports" />
    </QueryClientProvider>
  )
}

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
  const toggle = view.getByRole('switch', {
    name: 'Allow AI chat to use these tools',
  })
  expect(toggle.getAttribute('aria-checked')).toBe('false')
  fireEvent.click(toggle)
  expect(submitted).toBeUndefined()
  fireEvent.click(view.getByRole('button', { name: 'Save' }))
  await waitFor(() => expect(success).toHaveBeenCalledTimes(1))
  expect(submitted).toEqual({ actor_id: settings.actor_id, enabled: true })
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
  expect(view.getByRole('switch')).toBeTruthy()
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
  const toggle = await view.findByRole('switch')
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
