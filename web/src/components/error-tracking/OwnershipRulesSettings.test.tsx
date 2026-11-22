// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'

import type {
  OwnershipRuleList,
  OwnershipRuleRequest,
} from '@/api/client/types.gen'
import { listErrorOwnershipRulesQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { OwnershipRulesSettings } from './OwnershipRulesSettings'

installBrowserWindow()
afterEach(cleanup)

const RULES_PATH = '/projects/3/error-ownership-rules'

const ruleList = (patterns: string[]): OwnershipRuleList => ({
  data: patterns.map((pattern, i) => ({
    id: i + 1,
    pattern,
    owner_user_id: 7,
    owner_name: 'Sam Doe',
    position: i,
    created_at: '2026-11-01T10:00:00Z',
    updated_at: '2026-11-01T10:00:00Z',
  })),
})

function setup() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <OwnershipRulesSettings projectId={3} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const pattern = () =>
    view.getByLabelText('Path pattern 1') as HTMLInputElement
  return { client, view, pattern }
}

test('an edit made while a save runs is kept when the save answers', async () => {
  let answer: (() => void) | undefined
  const sent: OwnershipRuleRequest[][] = []
  mockApi({
    [`GET ${RULES_PATH}`]: () => jsonResponse(ruleList(['src/a/'])),
    [`PUT ${RULES_PATH}`]: async (request) => {
      const body = (await request.json()) as { rules: OwnershipRuleRequest[] }
      sent.push(body.rules)
      await new Promise<void>((resolve) => {
        answer = resolve
      })
      return jsonResponse(ruleList(body.rules.map((r) => r.pattern)))
    },
  })
  const { view, pattern } = setup()
  await view.findByLabelText('Path pattern 1')

  fireEvent.change(pattern(), { target: { value: 'src/b/' } })
  fireEvent.click(view.getByRole('button', { name: 'Save rules' }))
  await waitFor(() => expect(answer).toBeDefined())
  expect(sent[0][0].pattern).toBe('src/b/')

  // Typed while the save is in flight.
  fireEvent.change(pattern(), { target: { value: 'src/c/' } })
  answer?.()
  await view.findByRole('button', { name: 'Save rules' })
  await waitFor(() =>
    expect(
      (view.getByRole('button', { name: 'Save rules' }) as HTMLButtonElement)
        .disabled
    ).toBe(false)
  )
  expect(pattern().value).toBe('src/c/')
})

test('a save with no later edit shows the saved rules', async () => {
  mockApi({
    [`GET ${RULES_PATH}`]: () => jsonResponse(ruleList(['src/a/'])),
    [`PUT ${RULES_PATH}`]: async (request) => {
      const body = (await request.json()) as { rules: OwnershipRuleRequest[] }
      // The server answers with what it stored, trimmed.
      return jsonResponse(ruleList(body.rules.map((r) => r.pattern)))
    },
  })
  const { view, pattern } = setup()
  await view.findByLabelText('Path pattern 1')
  fireEvent.change(pattern(), { target: { value: '  src/b/  ' } })
  fireEvent.click(view.getByRole('button', { name: 'Save rules' }))
  await waitFor(() => expect(pattern().value).toBe('src/b/'))
})

test('a failed refresh keeps the unsaved rules on screen', async () => {
  let reads = 0
  mockApi({
    [`GET ${RULES_PATH}`]: () => {
      reads += 1
      return reads === 1
        ? jsonResponse(ruleList(['src/a/']))
        : jsonResponse({ title: 'Unavailable', status: 503 }, 503)
    },
  })
  const { client, view, pattern } = setup()
  await view.findByLabelText('Path pattern 1')
  fireEvent.change(pattern(), { target: { value: 'src/draft/' } })

  await client.invalidateQueries({
    queryKey: listErrorOwnershipRulesQueryKey({ path: { project_id: 3 } }),
  })
  await view.findByText(/could not be refreshed/)
  expect(reads).toBe(2)
  expect(pattern().value).toBe('src/draft/')
})
