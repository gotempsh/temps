// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  assistantParts,
  isTempsWriteToolName,
  reconcilePendingActionStatus,
  representedPendingActionIds,
  unrepresentedPendingActions,
  type ChatMessage,
} from './chat-message-parts'
import {
  appendLiveUserTurn,
  applicationHarnessPermissionNotice,
  chatApiPaths,
  chatFailureFromProblem,
  chatTurnActivityLabel,
  clearResolvedPermissionParts,
  conversationSnapshotPollInterval,
  conversationHistoryErrorMessage,
  ensureRunningAssistant,
  hasRunningServerTurn,
  isChatTranscriptNearBottom,
  needsTrailingActivityRow,
  parseChatFailure,
  permissionModeIsAuto,
  permissionModeOptionDisabled,
  permissionPollIsTerminal,
  serverElapsedDeciseconds,
  shouldShowAssistantActivityAfterContent,
  shouldShowLiveTurn,
  shouldSuppressPermissionPollEvent,
  toolLabel,
  turnStateNeedsResync,
} from './DebugChatPanel'
import { revokeAttachmentPreviews } from './attachment-previews'
import { chatDraftStorageKey } from './chat-runtime-options'

const completedTool = {
  id: 'tool-call-1',
  name: 'temps',
  arguments: '{"command":"projects get_projects"}',
  result: '{"status":200}',
}

describe('assistantParts', () => {
  test('keeps persisted assistant text visible when parts contain only tools', () => {
    const message: ChatMessage = {
      role: 'assistant',
      content: 'You have access to one project.',
      parts: [{ type: 'tool', tool: completedTool }],
    }

    expect(assistantParts(message)).toEqual([
      { type: 'tool', tool: completedTool },
      { type: 'text', text: 'You have access to one project.' },
    ])
  })

  test('does not duplicate text already represented in ordered parts', () => {
    const message: ChatMessage = {
      role: 'assistant',
      content: 'Before tool.After tool.',
      parts: [
        { type: 'text', text: 'Before tool.' },
        { type: 'tool', tool: completedTool },
        { type: 'text', text: 'After tool.' },
      ],
    }

    expect(assistantParts(message)).toEqual(message.parts!)
  })
})

describe('conversation-local client state', () => {
  test('polls conversation snapshots only when live updates are unavailable', () => {
    expect(conversationSnapshotPollInterval('connected', true)).toBe(false)
    expect(conversationSnapshotPollInterval('connecting', true)).toBe(false)
    expect(conversationSnapshotPollInterval('unavailable', false)).toBe(false)
    expect(conversationSnapshotPollInterval('unavailable', true)).toBe(2000)
  })

  test('uses durable conversation ids to isolate drafts in one context', () => {
    const common = {
      userScoped: true,
      contextType: 'application',
      contextId: 'app_1',
    }
    expect(
      chatDraftStorageKey({ ...common, conversationPublicId: 'thread_1' })
    ).not.toBe(
      chatDraftStorageKey({ ...common, conversationPublicId: 'thread_2' })
    )
    expect(chatDraftStorageKey(common)).toBe(
      'temps.ai.draft.user:context:application:app_1'
    )
  })

  test('revokes every local attachment preview', () => {
    const original = URL.revokeObjectURL
    const revoked: string[] = []
    URL.revokeObjectURL = (url) => revoked.push(url)
    try {
      revokeAttachmentPreviews([
        { preview_url: 'blob:first' },
        { preview_url: undefined },
        { preview_url: 'blob:second' },
      ])
      expect(revoked).toEqual(['blob:first', 'blob:second'])
    } finally {
      URL.revokeObjectURL = original
    }
  })
})

describe('tool card labels', () => {
  test('shows an MCP-qualified Temps command without expanding the card', () => {
    expect(
      toolLabel({
        id: 'mcp-tool-call',
        name: 'mcp__temps-chat__temps_write',
        arguments: JSON.stringify({
          command: 'external-services link_service_to_project --help',
        }),
        result: 'help text',
      })
    ).toBe('external-services link_service_to_project --help')
  })

  test('keeps the raw tool name when command arguments are malformed', () => {
    expect(
      toolLabel({
        id: 'malformed-tool-call',
        name: 'mcp__temps-chat__temps_write',
        arguments: '{not-json',
      })
    ).toBe('mcp__temps-chat__temps_write')
  })

  test('summarizes every command in a multi-command MCP plan', () => {
    expect(
      toolLabel({
        id: 'mcp-plan-call',
        name: 'mcp__temps-chat__temps_write',
        arguments: JSON.stringify({
          commands: [
            'external-services create_service --name postgres-18',
            'external-services link_service_to_project --id service-1',
          ],
        }),
      })
    ).toBe(
      '2 commands · external-services create_service --name postgres-18 → external-services link_service_to_project --id service-1'
    )
  })
})

describe('pending action recovery', () => {
  test('recognizes direct and harness-qualified proposal tool names', () => {
    expect(isTempsWriteToolName('temps_write')).toBe(true)
    expect(isTempsWriteToolName('mcp__temps-chat__temps_write')).toBe(true)
    expect(isTempsWriteToolName('untrusted__temps_write')).toBe(false)
    expect(isTempsWriteToolName('temps')).toBe(false)
    expect(isTempsWriteToolName('Bash')).toBe(false)
  })

  test('restores a proposal whose tool receipt was not persisted', () => {
    const messages: ChatMessage[] = [
      {
        role: 'assistant',
        content: 'Please confirm the proposal.',
        parts: [
          { type: 'text' as const, text: 'Please confirm the proposal.' },
        ],
      },
    ]
    const actions = [
      { public_id: 'action-missing', status: 'proposed' },
      { public_id: 'action-done', status: 'executed' },
    ]

    expect(unrepresentedPendingActions(messages, actions)).toEqual([actions[0]])
  })

  test('does not duplicate single or plan proposal cards', () => {
    const messages: ChatMessage[] = [
      {
        role: 'assistant',
        content: '',
        tools: [
          {
            id: 'single',
            name: 'temps_write',
            arguments: '{}',
            result: JSON.stringify({
              status: 'proposed',
              action_id: 'action-single',
            }),
          },
          {
            id: 'plan',
            name: 'mcp__temps-chat__temps_write',
            arguments: '{}',
            result: JSON.stringify({
              status: 'proposed_plan',
              steps: [
                { action_id: 'action-plan-1' },
                { action_id: 'action-plan-2' },
              ],
            }),
          },
        ],
      },
    ]

    expect([...representedPendingActionIds(messages)].sort()).toEqual([
      'action-plan-1',
      'action-plan-2',
      'action-single',
    ])
    expect(
      unrepresentedPendingActions(messages, [
        { public_id: 'action-single', status: 'proposed' },
        { public_id: 'action-plan-1', status: 'proposed' },
        { public_id: 'action-plan-2', status: 'proposed' },
      ])
    ).toEqual([])
  })

  test('removes a recovered action from the bottom tray after it fails', () => {
    const actions = [
      { public_id: 'action-failed', status: 'proposed', summary: 'Create DB' },
    ]

    const reconciled = reconcilePendingActionStatus(
      actions,
      'action-failed',
      'failed'
    )

    expect(unrepresentedPendingActions([], reconciled)).toEqual([])
    expect(reconciled[0]).toEqual({
      public_id: 'action-failed',
      status: 'failed',
      summary: 'Create DB',
    })
  })
})

describe('live connection state', () => {
  test('describes user work without exposing infrastructure or a thinking state', () => {
    expect(chatTurnActivityLabel('application', true)).toBe(
      'Preparing workspace'
    )
    expect(chatTurnActivityLabel('application', false)).toBe(
      'Working on your project'
    )
    expect(chatTurnActivityLabel('global', false)).toBe(
      'Working in your workspace'
    )
    expect(chatTurnActivityLabel('project', false)).toBe('Working')
  })

  test('keeps activity visible after the first token until the server turn completes', () => {
    expect(shouldShowAssistantActivityAfterContent(1, true)).toBe(true)
    expect(shouldShowAssistantActivityAfterContent(3, true)).toBe(true)
    expect(shouldShowAssistantActivityAfterContent(1, false)).toBe(false)
    expect(shouldShowAssistantActivityAfterContent(0, true)).toBe(false)
  })

  test('keeps elapsed activity time anchored to the server across remounts', () => {
    const startedAt = '2026-09-01T14:00:00.000Z'
    const firstMount = serverElapsedDeciseconds(
      startedAt,
      Date.parse('2026-09-01T14:00:12.300Z')
    )
    const refreshedMount = serverElapsedDeciseconds(
      startedAt,
      Date.parse('2026-09-01T14:00:14.800Z')
    )

    expect(firstMount).toBe(123)
    expect(refreshedMount).toBe(148)
    expect(refreshedMount).toBeGreaterThan(firstMount)
  })

  test('does not display invalid or future server timestamps as negative time', () => {
    expect(serverElapsedDeciseconds(null, 1_000)).toBe(0)
    expect(serverElapsedDeciseconds('invalid', 1_000)).toBe(0)
    expect(serverElapsedDeciseconds('2026-09-01T14:00:01.000Z', 0)).toBe(0)
  })

  test('keeps an accepted turn visible while the live wire connects', () => {
    expect(shouldShowLiveTurn(true, false, false)).toBe(true)
    // `false` here means retries have not been exhausted. Initial connection
    // setup and transient reconnects must not make a durable turn disappear.
    expect(shouldShowLiveTurn(false, true, false)).toBe(true)
    expect(shouldShowLiveTurn(true, false, true)).toBe(true)
    // Once reconnect retries are exhausted the explicit connection error owns
    // the UI, rather than presenting stale server activity as live thinking.
    expect(shouldShowLiveTurn(false, true, true)).toBe(false)
  })

  test('rehydrates an active server-owned turn after refresh', () => {
    expect(hasRunningServerTurn({ turn_status: 'running' })).toBe(true)
    expect(hasRunningServerTurn({ turn_status: 'completed' })).toBe(false)
    expect(hasRunningServerTurn(null)).toBe(false)
  })

  test('renders activity after persisted user history without an optimistic assistant', () => {
    expect(needsTrailingActivityRow(true, 'user')).toBe(true)
    expect(needsTrailingActivityRow(true, 'assistant')).toBe(false)
    expect(needsTrailingActivityRow(false, 'user')).toBe(false)
  })

  test('deduplicates the WebSocket echo of an optimistic submitted turn', () => {
    const optimistic: ChatMessage[] = [
      { role: 'user', content: 'ship it', client_turn_id: 'turn-1' },
      { role: 'assistant', content: '', client_turn_id: 'turn-1' },
    ]

    expect(
      appendLiveUserTurn(optimistic, {
        content: 'ship it',
        turn_id: 'turn-1',
      })
    ).toBe(optimistic)
    expect(
      appendLiveUserTurn([], { content: 'ship it', turn_id: 'turn-1' })
    ).toHaveLength(2)
  })

  test('preserves attachment metadata on a live user turn', () => {
    const messages = appendLiveUserTurn([], {
      content: '',
      turn_id: 'turn-with-image',
      attachments: [
        {
          id: 'attachment-1',
          name: 'design.png',
          mime_type: 'image/png',
          size_bytes: 4_096,
          sandbox_path:
            '/home/temps/workspace/.temps/chat-attachments/thread/attachment-1/design.png',
          is_image: true,
        },
      ],
    })

    expect(messages[0]?.attachments).toEqual([
      {
        id: 'attachment-1',
        name: 'design.png',
        mime_type: 'image/png',
        size_bytes: 4_096,
        sandbox_path:
          '/home/temps/workspace/.temps/chat-attachments/thread/attachment-1/design.png',
        is_image: true,
      },
    ])
  })

  test('hydrates a running turn with an assistant target for live events', () => {
    const history: ChatMessage[] = [{ role: 'user', content: 'continue' }]
    expect(
      ensureRunningAssistant(history, true).map((message) => message.role)
    ).toEqual(['user', 'assistant'])
    expect(ensureRunningAssistant(history, false)).toBe(history)
  })

  test('permission polling never suppresses lifecycle or assistant events', () => {
    expect(shouldSuppressPermissionPollEvent('user_message', 1)).toBe(true)
    expect(shouldSuppressPermissionPollEvent('turn_complete', 1)).toBe(false)
    expect(shouldSuppressPermissionPollEvent('text_delta', 1)).toBe(false)
    expect(shouldSuppressPermissionPollEvent('user_message', 0)).toBe(false)
  })

  test('permission polling trusts a terminal server turn even when counts match', () => {
    expect(permissionPollIsTerminal('completed', false)).toBe(true)
    expect(permissionPollIsTerminal('failed', false)).toBe(true)
    expect(permissionPollIsTerminal('running', false)).toBe(false)
    expect(permissionPollIsTerminal('completed', true)).toBe(false)
  })
})

describe('transcript follow mode', () => {
  test('follows output only while the reader remains near the bottom', () => {
    expect(
      isChatTranscriptNearBottom({
        scrollHeight: 1000,
        scrollTop: 528,
        clientHeight: 400,
      })
    ).toBe(true)
    expect(
      isChatTranscriptNearBottom({
        scrollHeight: 1000,
        scrollTop: 300,
        clientHeight: 400,
      })
    ).toBe(false)
  })
})

describe('application harness permissions', () => {
  test('hides the notice for Claude native approvals', () => {
    expect(
      applicationHarnessPermissionNotice('claude_cli', 'default')
    ).toBeNull()
  })

  test('explains unsupported provider approval and becomes quiet in Auto mode', () => {
    expect(
      applicationHarnessPermissionNotice('codex_cli', 'default')
    ).toContain('Choose Auto')
    expect(
      applicationHarnessPermissionNotice('codex_cli', 'full-access')
    ).toBeNull()
    expect(applicationHarnessPermissionNotice('codex_cli', 'auto')).toBeNull()
  })
})

describe('active-turn permission changes', () => {
  test('reconciles a running snapshot so missed approval cards are restored', () => {
    expect(turnStateNeedsResync('running')).toBe(true)
    expect(turnStateNeedsResync('completed')).toBe(false)
  })

  test('only allows elevation to Auto while a turn is running', () => {
    expect(permissionModeIsAuto('full-access')).toBe(true)
    expect(permissionModeIsAuto('default')).toBe(false)
    expect(permissionModeOptionDisabled(true, 'default')).toBe(true)
    expect(permissionModeOptionDisabled(true, 'accept-edits')).toBe(true)
    expect(permissionModeOptionDisabled(true, 'full-access')).toBe(false)
    expect(permissionModeOptionDisabled(false, 'default')).toBe(false)
  })

  test('removes a server-consumed approval without losing other output', () => {
    const messages: ChatMessage[] = [
      {
        role: 'assistant',
        content: 'I am working on it.',
        parts: [
          { type: 'text', text: 'I am working on it.' },
          {
            type: 'permission',
            permission: {
              id: 'permission-1',
              kind: 'tool_approval',
              tool_name: 'Bash',
              input: { command: 'npm install' },
            },
          },
        ],
      },
    ]

    expect(clearResolvedPermissionParts(messages, ['permission-1'])).toEqual([
      {
        role: 'assistant',
        content: 'I am working on it.',
        parts: [{ type: 'text', text: 'I am working on it.' }],
      },
    ])
  })

  test('keeps unrelated questions and platform approvals visible', () => {
    const messages: ChatMessage[] = [
      {
        role: 'assistant',
        content: '',
        parts: [
          {
            type: 'permission',
            permission: {
              id: 'auto-approved',
              kind: 'tool_approval',
              tool_name: 'Bash',
              input: {},
            },
          },
          {
            type: 'permission',
            permission: {
              id: 'still-pending',
              kind: 'question',
              tool_name: 'AskUserQuestion',
              input: {},
            },
          },
        ],
      },
    ]

    const [message] = clearResolvedPermissionParts(messages, ['auto-approved'])
    expect(message.parts).toHaveLength(1)
    expect(message.parts?.[0]).toMatchObject({
      type: 'permission',
      permission: { id: 'still-pending' },
    })
  })
})

describe('chat ownership routes', () => {
  test('uses user-rooted routes for AI workspace threads', () => {
    expect(chatApiPaths(true)).toEqual({
      conversations: '/api/ai/conversations',
      pendingActions: '/api/ai/pending-actions',
    })
  })

  test('keeps legacy project chat routes scoped to their project', () => {
    expect(chatApiPaths(false, 42)).toEqual({
      conversations: '/api/projects/42/ai/conversations',
      pendingActions: '/api/projects/42/ai/pending-actions',
    })
  })
})

describe('conversation history failures', () => {
  test('makes connectivity failures explicit without claiming history is empty', () => {
    expect(conversationHistoryErrorMessage(503)).toBe(
      'Couldn’t load this conversation (HTTP 503). Its messages remain stored in Temps; reconnect and try again.'
    )
    expect(conversationHistoryErrorMessage()).toContain(
      'messages remain stored in Temps'
    )
  })
})

describe('public chat failures', () => {
  test('renders a structured actionable reason from the live wire', () => {
    expect(
      parseChatFailure(
        JSON.stringify({
          code: 'tool_configuration_unreadable',
          title: 'Application tools could not start',
          detail:
            "The sandbox could not read Temps' temporary tool configuration. Restart the application sandbox and retry.",
          retryable: true,
        })
      )
    ).toEqual({
      code: 'tool_configuration_unreadable',
      title: 'Application tools could not start',
      detail:
        "The sandbox could not read Temps' temporary tool configuration. Restart the application sandbox and retry.",
      retryable: true,
    })
  })

  test('never renders raw legacy provider diagnostics or secret paths', () => {
    const raw =
      "AI provider error: EACCES open '/run/secrets/temps-chat-mcp.json' token=private"
    const parsed = parseChatFailure(raw)

    expect(parsed.title).toBe('AI harness failed')
    expect(JSON.stringify(parsed)).not.toContain('/run/secrets')
    expect(JSON.stringify(parsed)).not.toContain('token=private')
  })

  test('keeps safe problem details but rejects private-looking ones', () => {
    expect(
      chatFailureFromProblem(
        {
          title: 'Selected model unavailable',
          detail:
            'Refresh the model list, choose an available model, and retry.',
        },
        500
      ).title
    ).toBe('Selected model unavailable')

    const privateProblem = chatFailureFromProblem(
      {
        title: 'Internal Server Error',
        detail: "Could not open '/home/temps/.config/provider.json'",
      },
      500
    )
    expect(privateProblem.code).toBe('harness_failed')
    expect(privateProblem.detail).not.toContain('/home/temps')
  })
})
