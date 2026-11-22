// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { BlueprintDocument } from '@/api/client/types.gen'
import {
  acceptedMessage,
  deployAllMessage,
  isProposeBlueprintTool,
  isSettling,
  parseBlueprintProposal,
  rejectedMessage,
} from './chat-blueprint'

const document: BlueprintDocument = {
  summary: 'Team tasks',
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
      name: 'Web',
      purpose: 'Pages',
      kind: 'app',
      role: 'web',
      runtime: 'preset',
      stack: 'Vite',
    },
    {
      id: 'api',
      name: 'API',
      purpose: 'Tasks',
      kind: 'app',
      role: 'api',
      runtime: 'preset',
      stack: 'Bun + Hono',
    },
    {
      id: 'db',
      name: 'DB',
      purpose: 'Rows',
      kind: 'data',
      engine: 'postgres',
      holds: ['tasks'],
    },
  ],
  flows: [
    { from: 'browser', to: 'web', kind: 'http', label: 'pages' },
    { from: 'web', to: 'api', kind: 'http', label: '/api/tasks' },
    { from: 'api', to: 'db', kind: 'query', label: 'tasks' },
  ],
}

describe('isProposeBlueprintTool', () => {
  test('matches the tool with or without a server prefix', () => {
    expect(isProposeBlueprintTool('temps_propose_blueprint')).toBe(true)
    expect(
      isProposeBlueprintTool('mcp__temps-chat__temps_propose_blueprint')
    ).toBe(true)
    expect(isProposeBlueprintTool('temps_bind_blueprint')).toBe(false)
  })
})

describe('parseBlueprintProposal', () => {
  test('reads a recorded proposal', () => {
    const receipt = parseBlueprintProposal(
      JSON.stringify({
        status: 'proposed',
        application_id: 'app_1',
        workspace_name: 'Team tasks',
        workspace_created: true,
        version: 2,
        change_summary: 'First design',
        document: { ...document, flows: undefined },
        next: 'Stop and wait',
      })
    )
    expect(receipt).toMatchObject({
      applicationId: 'app_1',
      workspaceName: 'Team tasks',
      workspaceCreated: true,
      version: 2,
      changeSummary: 'First design',
    })
    expect(receipt?.document.flows).toEqual([])
  })

  test('a refusal, a running call or a malformed result is not a proposal', () => {
    expect(parseBlueprintProposal(null)).toBeNull()
    expect(
      parseBlueprintProposal(
        'The design was not recorded: part web needs a stack'
      )
    ).toBeNull()
    expect(
      parseBlueprintProposal(JSON.stringify({ status: 'proposed', version: 1 }))
    ).toBeNull()
    expect(
      parseBlueprintProposal(
        JSON.stringify({
          status: 'proposed',
          application_id: 'app_1',
          version: 1,
          document: { summary: 'x', components: [{ id: 'a', kind: 'robot' }] },
        })
      )
    ).toBeNull()
  })
})

describe('messages to the chat', () => {
  test('accepting tells the assistant to build', () => {
    expect(acceptedMessage(3)).toContain('version 3')
    expect(acceptedMessage(3)).toContain('Build it')
  })

  test('rejecting passes the reason on, or asks the assistant to ask', () => {
    expect(rejectedMessage(2, 'Use Redis Streams.')).toContain(
      'Use Redis Streams.'
    )
    expect(rejectedMessage(2, '  ')).toContain('Ask me what to change')
  })

  test('deploy all lists the apps with the API before the front end', () => {
    const message = deployAllMessage(document) ?? ''
    expect(message.indexOf('`api`')).toBeLessThan(message.indexOf('`web`'))
    expect(message).not.toContain('`db`')
  })

  test('a design with no apps has nothing to deploy', () => {
    expect(
      deployAllMessage({
        ...document,
        components: [document.components[3]],
        flows: [],
      })
    ).toBeNull()
  })
})

test('a part still building keeps the blueprint refreshing', () => {
  expect(isSettling({ web: { status: 'building' } })).toBe(true)
  expect(isSettling({ web: { status: 'live' }, db: { status: 'live' } })).toBe(
    false
  )
})
