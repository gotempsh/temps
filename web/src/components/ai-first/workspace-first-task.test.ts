// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ConversationResponse, SendMessageRequest } from '@/api/client'
import { workspaceFirstTask } from './workspace-first-task'

const conversation = {
  public_id: 'thread_test',
  ai_provider: 'codex_cli',
} as ConversationResponse
const selection = {
  providerId: 'codex_cli',
  modelId: 'model_test',
  thinkingOptionId: 'high',
  permissionModeId: 'on-request',
}

describe('workspace first task', () => {
  test('creates a thread and sends the prompt with the chosen runtime options', async () => {
    const sent: SendMessageRequest[] = []
    const result = await workspaceFirstTask().start({
      prompt: '  Build a landing page  ',
      selection,
      turnId: () => 'turn_test',
      createThread: async () => conversation,
      recoverThread: async () => null,
      send: async (id, body) => {
        expect(id).toBe('thread_test')
        sent.push(body)
      },
    })
    expect(result).toBe(conversation)
    expect(sent).toEqual([
      {
        content: 'Build a landing page',
        turn_id: 'turn_test',
        ai_model: 'model_test',
        ai_thinking_level: 'high',
        ai_permission_mode: 'on-request',
      },
    ])
  })

  test('retries a failed send without creating another thread or changing its idempotency body', async () => {
    const task = workspaceFirstTask()
    let creates = 0
    const sent: SendMessageRequest[] = []
    const options = {
      prompt: 'Build a landing page',
      selection,
      turnId: () => 'turn_test',
      createThread: async () => {
        creates += 1
        return conversation
      },
      recoverThread: async () => null,
      send: async (_id: string, body: SendMessageRequest) => {
        sent.push(body)
        if (sent.length === 1) throw new Error('Connection lost')
      },
    }
    await expect(task.start(options)).rejects.toThrow('Connection lost')
    await task.start({
      ...options,
      selection: { ...selection, modelId: 'changed' },
      prompt: 'changed',
    })
    expect(creates).toBe(1)
    expect(sent).toHaveLength(2)
    expect(sent[1]).toBe(sent[0])
    expect(sent[1].content).toBe('Build a landing page')
  })

  test('recovers a created thread after a lost creation response', async () => {
    const task = workspaceFirstTask()
    let creates = 0
    const options = {
      prompt: 'Task',
      selection,
      turnId: () => 'turn_test',
      createThread: async () => {
        creates += 1
        throw new Error('Response lost')
      },
      recoverThread: async () => conversation,
      send: async () => {},
    }
    await expect(task.start(options)).rejects.toThrow('Response lost')
    expect(await task.start(options)).toBe(conversation)
    expect(creates).toBe(1)
  })

  test('does not blindly recreate a thread when recovery is ambiguous', async () => {
    const task = workspaceFirstTask()
    let creates = 0
    const options = {
      prompt: 'Task',
      selection,
      turnId: () => 'turn_test',
      createThread: async () => {
        creates += 1
        throw new Error('Response lost')
      },
      recoverThread: async () => null,
      send: async () => {
        throw new Error('must not send')
      },
    }
    await expect(task.start(options)).rejects.toThrow('Response lost')
    await expect(task.start(options)).rejects.toThrow('Could not confirm')
    expect(creates).toBe(1)
  })

  test('rejects empty prompts before creating resources', async () => {
    await expect(
      workspaceFirstTask().start({
        prompt: ' \n ',
        selection,
        turnId: () => 'unused',
        createThread: async () => {
          throw new Error('must not create')
        },
        recoverThread: async () => null,
        send: async () => {},
      })
    ).rejects.toThrow('Describe your first task')
  })
})
