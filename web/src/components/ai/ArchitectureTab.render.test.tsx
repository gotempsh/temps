// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  getBlueprintQueryKey,
  getChatWorkspaceQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  BlueprintDocument,
  BlueprintResponse,
  ChatWorkspaceResponse,
} from '@/api/client/types.gen'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ArchitectureTab } from './ArchitectureTab'
import { BlueprintProposalCard } from './BlueprintProposalCard'
import { proposalState } from './chat-blueprint'

const CHAT = 'conv_1'
const APP = 'app_1'

const document: BlueprintDocument = {
  summary: 'Tasks for small teams',
  components: [
    {
      id: 'browser',
      name: 'Browser',
      purpose: 'Use it',
      kind: 'client',
      surface: 'browser',
    },
    {
      id: 'web',
      name: 'Task board',
      purpose: 'Pages',
      kind: 'app',
      role: 'web',
      runtime: 'preset',
      stack: 'Vite',
    },
    {
      id: 'db',
      name: 'Task store',
      purpose: 'Rows',
      kind: 'data',
      engine: 'postgres',
      holds: ['tasks'],
    },
  ],
  flows: [
    { from: 'browser', to: 'web', kind: 'http', label: 'pages' },
    { from: 'web', to: 'db', kind: 'query', label: 'tasks' },
  ],
}

function version(n: number, status: 'proposed' | 'accepted') {
  return {
    version: n,
    document,
    change_summary: `Version ${n}`,
    status,
    proposed_by_assistant: true,
    created_at: '2026-10-20T08:00:00Z',
    decided_at: status === 'accepted' ? '2026-10-20T08:05:00Z' : null,
  }
}

function render(
  node: ReactNode,
  {
    workspace,
    blueprint,
  }: { workspace?: ChatWorkspaceResponse; blueprint?: BlueprintResponse }
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  if (workspace) {
    client.setQueryData(
      getChatWorkspaceQueryKey({ path: { public_id: CHAT } }),
      workspace
    )
  }
  if (blueprint) {
    client.setQueryData(
      getBlueprintQueryKey({ path: { public_id: APP } }),
      blueprint
    )
  }
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>
  )
}

const inWorkspace: ChatWorkspaceResponse = {
  workspace: { public_id: APP, name: 'Team tasks' },
  chats: [],
  joinable: true,
}

describe('proposalState', () => {
  const blueprint: BlueprintResponse = {
    application_id: APP,
    workspace_name: 'Team tasks',
    accepted: version(1, 'accepted'),
    proposal: version(2, 'proposed'),
    bindings: {},
    live: {},
  }

  test('tells an open proposal from a decided or replaced one', () => {
    expect(proposalState(blueprint, 2)).toBe('open')
    expect(proposalState(blueprint, 1)).toBe('accepted')
    expect(proposalState({ ...blueprint, proposal: null }, 3)).toBe('rejected')
    expect(proposalState(blueprint, 0)).toBe('superseded')
    expect(proposalState(undefined, 2)).toBe('unknown')
  })
})

describe('ArchitectureTab', () => {
  test('a project chat says why it has no architecture', () => {
    const markup = render(<ArchitectureTab conversationPublicId={CHAT} />, {
      workspace: {
        chats: [],
        joinable: false,
        reason: 'This chat is about one project.',
      },
    })
    expect(markup).toContain('No architecture in a project chat')
    expect(markup).toContain('This chat is about one project.')
  })

  test('a chat with no design shows what one is and how to get it', () => {
    const markup = render(<ArchitectureTab conversationPublicId={CHAT} />, {
      workspace: { chats: [], joinable: true },
    })
    expect(markup).toContain('Design the application before building it')
    expect(markup).toContain('For example')
    expect(markup).toContain('Ask the assistant to design it')
    expect(markup).toContain('Draw it myself')
  })

  test('an accepted design is drawn with its live parts and Deploy all', () => {
    const markup = render(<ArchitectureTab conversationPublicId={CHAT} />, {
      workspace: inWorkspace,
      blueprint: {
        application_id: APP,
        workspace_name: 'Team tasks',
        accepted: version(1, 'accepted'),
        proposal: null,
        bindings: { web: { project_id: 7 } },
        live: {
          web: {
            status: 'live',
            name: 'task-board',
            project_slug: 'task-board',
          },
        },
      },
    })
    expect(markup).toContain('Team tasks')
    expect(markup).toContain('version 1')
    expect(markup).toContain('Task board')
    expect(markup).toContain('Task store')
    expect(markup).toContain('Deploy all')
    expect(markup).toContain('Task board, App, Live')
  })

  test('a waiting proposal asks for the decision above the accepted design', () => {
    const markup = render(<ArchitectureTab conversationPublicId={CHAT} />, {
      workspace: inWorkspace,
      blueprint: {
        application_id: APP,
        workspace_name: 'Team tasks',
        accepted: version(1, 'accepted'),
        proposal: version(2, 'proposed'),
        bindings: {},
        live: {},
      },
    })
    expect(markup).toContain('Proposed architecture')
    expect(markup).toContain('Accept and build')
    expect(markup.indexOf('Accept and build')).toBeLessThan(
      markup.indexOf('Deploy all')
    )
  })
})

describe('BlueprintProposalCard', () => {
  const receipt = {
    applicationId: APP,
    workspaceName: 'Team tasks',
    workspaceCreated: true,
    version: 2,
    changeSummary: 'First design',
    document,
  }

  test('an open proposal offers to accept it or change something', () => {
    const markup = render(
      <BlueprintProposalCard conversationPublicId={CHAT} receipt={receipt} />,
      {
        workspace: inWorkspace,
        blueprint: {
          application_id: APP,
          workspace_name: 'Team tasks',
          accepted: null,
          proposal: version(2, 'proposed'),
          bindings: {},
          live: {},
        },
      }
    )
    expect(markup).toContain('Accept and build')
    expect(markup).toContain('Change something')
    expect(markup).toContain('Nothing is built until you accept.')
  })

  test('a decided proposal shows the decision instead of the buttons', () => {
    const markup = render(
      <BlueprintProposalCard conversationPublicId={CHAT} receipt={receipt} />,
      {
        workspace: inWorkspace,
        blueprint: {
          application_id: APP,
          workspace_name: 'Team tasks',
          accepted: version(2, 'accepted'),
          proposal: null,
          bindings: {},
          live: {},
        },
      }
    )
    expect(markup).toContain('Accepted')
    expect(markup).not.toContain('Accept and build')
  })
})
