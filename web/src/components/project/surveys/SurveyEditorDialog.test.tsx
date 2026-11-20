// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { SurveyResponse } from '@/api/client'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { SurveyEditorDialog } from './SurveyEditorDialog'

installBrowserWindow()
const realFetch = globalThis.fetch
const realBaseUrl = client.getConfig().baseUrl
afterEach(() => {
  cleanup()
  globalThis.fetch = realFetch
  client.setConfig({ baseUrl: realBaseUrl })
})

function survey(name: string, updatedAt: string): SurveyResponse {
  return {
    id: 3,
    project_id: 1,
    name,
    description: null,
    status: 'draft',
    questions: [{ id: 'q_score', kind: 'nps', prompt: 'How likely?' }],
    targeting: { sample_rate: 1, once_per_visitor: true },
    created_at: '2026-11-20T09:00:00Z',
    updated_at: updatedAt,
  } as SurveyResponse
}

test('a save carries the revision the draft was opened from, not a newer one', async () => {
  const sent: Record<string, unknown>[] = []
  client.setConfig({ baseUrl: 'http://console.example.test/api' })
  globalThis.fetch = (async (request: Request) => {
    sent.push((await request.json()) as Record<string, unknown>)
    return new Response(
      JSON.stringify({ type: 'about:blank', title: 'Conflict', status: 409 }),
      { status: 409, headers: { 'Content-Type': 'application/problem+json' } }
    )
  }) as unknown as typeof fetch
  const queryClient = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  const tree = (current: SurveyResponse) => (
    <QueryClientProvider client={queryClient}>
      <SurveyEditorDialog
        projectId={1}
        survey={current}
        open
        onOpenChange={() => {}}
        onSaved={() => {}}
      />
    </QueryClientProvider>
  )
  const view = render(tree(survey('Ours', '2026-11-20T10:00:00Z')))
  // Another operator saves; the detail query refreshes under the open dialog.
  view.rerender(tree(survey('Theirs', '2026-11-20T10:05:00Z')))

  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
  })
  expect(sent).toHaveLength(1)
  expect(sent[0]?.expected_updated_at).toBe('2026-11-20T10:00:00Z')
  expect(sent[0]?.name).toBe('Ours')
})
