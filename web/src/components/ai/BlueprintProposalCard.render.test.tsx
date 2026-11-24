// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getBlueprintQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  BlueprintDocument,
  BlueprintResponse,
  BlueprintVersionResponse,
} from '@/api/client/types.gen'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { BlueprintProposalCard } from './BlueprintProposalCard'

const document: BlueprintDocument = {
  summary: 'A notes app with a database.',
  components: [
    { id: 'web', kind: 'app', name: 'Notes UI', role: 'web' },
    { id: 'db', kind: 'data', name: 'Notes Database', engine: 'postgres' },
  ],
  flows: [{ from: 'web', to: 'db', kind: 'query', label: 'notes' }],
} as BlueprintDocument

const version = (
  number: number,
  status: BlueprintVersionResponse['status']
): BlueprintVersionResponse => ({
  version: number,
  status,
  change_summary: '',
  created_at: '2026-01-01T00:00:00Z',
  proposed_by_assistant: true,
  document,
})

function render(blueprint: BlueprintResponse) {
  const client = new QueryClient()
  client.setQueryData(
    getBlueprintQueryKey({ path: { public_id: 'app-1' } }),
    blueprint
  )
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <BlueprintProposalCard
        conversationPublicId="chat-1"
        receipt={{
          applicationId: 'app-1',
          workspaceName: 'Notes',
          workspaceCreated: false,
          version: 2,
          changeSummary: '',
          document,
        }}
      />
    </QueryClientProvider>
  )
}

const base = {
  application_id: 'app-1',
  workspace_name: 'Notes',
  bindings: {},
  live: {},
}

test('an open proposal can be accepted, changed, or rejected outright', () => {
  const markup = render({
    ...base,
    accepted: version(1, 'accepted'),
    proposal: version(2, 'proposed'),
  })
  expect(markup).toContain('Accept and build')
  expect(markup).toContain('Change something')
  expect(markup).toContain('>Reject<')
  expect(markup).toContain(
    'title="Discard this proposal without asking for another one"'
  )
})

test('a rejected proposal offers no decision and says it was rejected', () => {
  const markup = render({ ...base, accepted: version(1, 'accepted') })
  expect(markup).toContain('Rejected')
  expect(markup).not.toContain('Accept and build')
  expect(markup).not.toContain('>Reject<')
})
