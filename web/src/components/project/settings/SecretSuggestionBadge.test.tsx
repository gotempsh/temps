// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  SecretSuggestionBadge,
  type SecretSuggestionBadgeProps,
} from './SecretSuggestionBadge'

installBrowserWindow()
afterEach(cleanup)

const SUGGESTION: SecretSuggestionBadgeProps['suggestion'] = {
  message: 'DATABASE_URL contains a password in its connection string',
  reason: 'connection_string_password',
}

function renderBadge(props: Partial<SecretSuggestionBadgeProps> = {}) {
  const events = { converted: 0, dismissed: 0 }
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <SecretSuggestionBadge
        projectId={4}
        variableId={9}
        variableKey="DATABASE_URL"
        suggestion={SUGGESTION}
        environmentIds={[1, 2]}
        includeInPreview
        onConverted={() => events.converted++}
        onDismissed={() => events.dismissed++}
        {...props}
      />
    </QueryClientProvider>
  )
  return { view, events }
}

test('renders nothing when there is no suggestion', () => {
  const api = mockApi({})
  const { view } = renderBadge({ suggestion: null })
  expect(view.container.innerHTML).toBe('')
  api.restore()
})

test('"It\'s not a secret" dismisses the suggestion', async () => {
  const api = mockApi({
    'DELETE /projects/4/env-vars/9/secret-suggestion': () =>
      new Response(null, { status: 204 }),
  })
  const { view, events } = renderBadge()

  fireEvent.click(view.getByRole('button', { name: /looks like a credential/ }))
  fireEvent.click(
    await view.findByRole('button', { name: "It's not a secret" })
  )

  await waitFor(() => expect(events.dismissed).toBe(1))
  expect(api.called('DELETE', '/projects/4/env-vars/9/secret-suggestion')).toBe(
    true
  )
  api.restore()
})

test('converting keeps the bindings and never sends a value', async () => {
  let body: Record<string, unknown> | undefined
  const api = mockApi({
    'PUT /projects/4/env-vars/9': async (request) => {
      body = (await request.json()) as Record<string, unknown>
      return jsonResponse({})
    },
  })
  const { view, events } = renderBadge()

  fireEvent.click(view.getByRole('button', { name: /looks like a credential/ }))
  fireEvent.click(await view.findByRole('button', { name: 'Store as secret' }))
  fireEvent.click(
    await view.findByRole('button', { name: 'Convert to secret' })
  )

  await waitFor(() => expect(events.converted).toBe(1))
  expect(body).toEqual({
    key: 'DATABASE_URL',
    environment_ids: [1, 2],
    include_in_preview: true,
    is_secret: true,
  })
  expect(body).not.toHaveProperty('value')
  api.restore()
})
