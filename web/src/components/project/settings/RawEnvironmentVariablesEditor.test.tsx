// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import type {
  EnvironmentResponse,
  EnvironmentVariableResponse,
} from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { RawEnvironmentVariablesEditor } from './RawEnvironmentVariablesEditor'

installBrowserWindow()
afterEach(cleanup)

const production = {
  id: 1,
  name: 'production',
  slug: 'production',
  is_preview: false,
} as EnvironmentResponse

const existing = {
  id: 7,
  key: 'API_URL',
  environments: [{ id: 1, name: 'production', main_url: '' }],
  include_in_preview: false,
  is_secret: false,
  created_at: 0,
  updated_at: 0,
} as unknown as EnvironmentVariableResponse

test('a partly failed apply keeps the editor open with the edited text', async () => {
  const api = mockApi({
    'GET /projects/4/env-vars/API_URL/value': () =>
      jsonResponse({ value: 'https://old.example.test' }),
    'PUT /projects/4/env-vars/7': () => jsonResponse({}),
    'POST /projects/4/env-vars': () =>
      jsonResponse({ title: 'Bad Request', detail: 'key is reserved' }, 400),
  })
  const closes: boolean[] = []
  const applied: string[] = []
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <RawEnvironmentVariablesEditor
        open
        onOpenChange={(open) => closes.push(open)}
        projectId={4}
        variables={[existing]}
        environments={[production]}
        initialEnvironmentId={1}
        onApplied={(title) => applied.push(title)}
      />
    </QueryClientProvider>
  )

  const editor = (await view.findByLabelText(
    'Environment variables for production'
  )) as HTMLTextAreaElement
  await waitFor(() => expect(editor.value).toContain('old.example.test'))
  const edited = 'API_URL=https://new.example.test\nNEW_KEY=typed-by-hand\n'
  fireEvent.change(editor, { target: { value: edited } })
  fireEvent.click(view.getByRole('button', { name: 'Review 2 changes' }))
  fireEvent.click(view.getByRole('button', { name: 'Apply 2 changes' }))

  await waitFor(() => expect(applied).toHaveLength(1))
  // Back in the editor, with the line that failed still there to retry.
  const after = (await view.findByLabelText(
    'Environment variables for production'
  )) as HTMLTextAreaElement
  expect(after.value).toBe(edited)
  expect(closes).not.toContain(false)
  api.restore()
})
