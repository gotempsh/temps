// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import {
  assistantParts,
  upsertMessageTool,
  toolExecutionState,
  type ChatMessage,
} from './chat-message-parts'

const tool = { id: 'bash-1', name: 'bash', arguments: '{"command":"pwd"}' }
describe('complete tool visibility', () => {
  test('uses a persisted terminal result when the ordered part is incomplete', () => {
    const parts = assistantParts({
      role: 'assistant',
      content: '',
      parts: [{ type: 'tool', tool }],
      tools: [{ ...tool, result: 'done' }],
    })
    expect(parts[0]).toEqual({
      type: 'tool',
      tool: { ...tool, result: 'done' },
    })
  })
  test('keeps tools omitted from ordered history parts without duplicating known tools', () => {
    const parts = assistantParts({
      role: 'assistant',
      content: 'Done',
      parts: [
        { type: 'text', text: 'Done' },
        { type: 'tool', tool },
      ],
      tools: [tool, { ...tool, id: 'bash-2' }],
    })
    expect(parts.filter((part) => part.type === 'tool')).toHaveLength(2)
  })
  test('shows a result when its start event was missed', () => {
    const message = upsertMessageTool(
      { role: 'assistant', content: '' },
      { ...tool, result: '/workspace' }
    )
    expect(message.tools?.[0].result).toBe('/workspace')
    expect(assistantParts(message)).toHaveLength(1)
  })
  test('duplicate or delayed starts cannot clear terminal results', () => {
    let message: ChatMessage = { role: 'assistant', content: '' }
    message = upsertMessageTool(message, { ...tool, result: 'ok' })
    message = upsertMessageTool(message, { ...tool, result: undefined })
    expect(message.tools).toHaveLength(1)
    expect(message.tools?.[0].result).toBe('ok')
    expect(assistantParts(message)).toHaveLength(1)
  })
  test('tracks execution status on each tool, including empty success receipts', () => {
    expect(toolExecutionState(tool)).toBe('running')
    expect(toolExecutionState({ ...tool, result: null })).toBe('running')
    expect(toolExecutionState({ ...tool, result: '' })).toBe('completed')
    expect(toolExecutionState({ ...tool, result: '0 errors found' })).toBe('completed')
    for (const result of ['{"is_error":true}', '{"error":"Permission denied"}', '{"status":"failed"}', '{"exit_code":7}', 'connection refused\nProcess exited with code 7.']) {
      expect(toolExecutionState({ ...tool, result })).toBe('failed')
    }
  })
})
