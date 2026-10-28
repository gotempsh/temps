// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ShieldTokensPanel } from './ShieldTokensPanel'

installBrowserWindow()
afterEach(cleanup)

const TOKENS_PATH = '/projects/3/environments/7/shield/tokens'
const SECRET = 'tst_11_example-secret-value'

const TOKEN = {
  id: 11,
  name: 'Uptime monitor',
  scope: 'environment',
  environment_id: 7,
  created_by: 1,
  created_at: '2026-10-27T10:00:00Z',
  last_used_at: null,
}

/** The dialog's submit button: the last "Create token" on the page. */
function submitButton(view: ReturnType<typeof renderPanel>) {
  const buttons = view.getAllByRole('button', { name: 'Create token' })
  return buttons[buttons.length - 1]
}

function renderPanel() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <ShieldTokensPanel
        path={{ project_id: 3, environment_id: 7 }}
        environmentName="production"
      />
    </QueryClientProvider>
  )
}

test('a new token is shown once, then gone when the dialog closes', async () => {
  let created = false
  let sentBody: unknown = null
  const api = mockApi({
    [`GET ${TOKENS_PATH}`]: () => jsonResponse(created ? [TOKEN] : []),
    [`POST ${TOKENS_PATH}`]: async (request) => {
      sentBody = await request.json()
      created = true
      return jsonResponse({ ...TOKEN, secret: SECRET }, 201)
    },
  })
  const view = renderPanel()

  await view.findByText('No bypass tokens yet')
  fireEvent.click(view.getByRole('button', { name: 'Create token' }))
  const name = await view.findByLabelText('Name')
  fireEvent.change(name, { target: { value: 'Uptime monitor' } })
  fireEvent.click(submitButton(view))

  await view.findByText(SECRET)
  expect(sentBody).toEqual({ name: 'Uptime monitor', project_wide: false })
  expect(view.getByRole('button', { name: 'Copy bypass token' })).toBeTruthy()
  // How to send it, and where it applies.
  view.getByText(
    `curl -H "X-Temps-Shield-Token: ${SECRET}" https://your-app.example.com/`
  )
  view.getByText(
    /never challenges or blocks a request that carries it, in production/
  )

  fireEvent.click(view.getByRole('button', { name: 'I have saved the token' }))
  await view.findByText('Uptime monitor')
  await waitFor(() => expect(view.queryByText(SECRET)).toBeNull())
  api.restore()
})

test('an empty name is refused next to the field, without a request', async () => {
  const api = mockApi({
    [`GET ${TOKENS_PATH}`]: () => jsonResponse([]),
  })
  const view = renderPanel()

  await view.findByText('No bypass tokens yet')
  fireEvent.click(view.getByRole('button', { name: 'Create token' }))
  await view.findByLabelText('Name')
  fireEvent.click(submitButton(view))

  await view.findByText('Name the token after what will send it.')
  expect(api.called('POST', TOKENS_PATH)).toBe(false)
  api.restore()
})
