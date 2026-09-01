// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  assistantParts,
  representedPendingActionIds,
  unrepresentedPendingActions,
  type ChatMessage,
} from './chat-message-parts'

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

describe('pending action recovery', () => {
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
            name: 'temps_write',
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
})
