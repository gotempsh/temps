// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import {
  ASSISTANT_EXAMPLE_PROMPTS,
  NO_PROVIDER_REASON,
  OPENROUTER_SETUP_PATH,
  ASK_BEFORE_CHANGES_MODE,
  RUN_CHANGES_AUTOMATICALLY_MODE,
  assistantReadiness,
  asksBeforeChanges,
  chatTint,
  isAssistantConversation,
  permissionModeForAsk,
  setupPathFromProblem,
  loadLastAssistantKeyId,
  loadLastAssistantModel,
  saveLastAssistantKeyId,
  saveLastAssistantModel,
} from './assistant-state'

describe('assistantReadiness', () => {
  test('is ready when a gateway provider is available', () => {
    expect(assistantReadiness({ gateway_available: true })).toEqual({
      state: 'ready',
    })
  })

  test('a ready host harness without a gateway key does not count', () => {
    const readiness = assistantReadiness({
      gateway_available: false,
      reason: null,
      setup_path: null,
    })
    expect(readiness.state).toBe('unconfigured')
  })

  test('uses the server reason and setup path when they are usable', () => {
    expect(
      assistantReadiness({
        gateway_available: false,
        reason: 'No AI provider is configured.',
        setup_path: '/ai-gateway?add=openrouter',
      })
    ).toEqual({
      state: 'unconfigured',
      reason: 'No AI provider is configured.',
      setupPath: '/ai-gateway?add=openrouter',
    })
  })

  test('falls back to the default reason and OpenRouter path when omitted', () => {
    expect(assistantReadiness(undefined)).toEqual({
      state: 'unconfigured',
      reason: NO_PROVIDER_REASON,
      setupPath: OPENROUTER_SETUP_PATH,
    })
    expect(
      assistantReadiness({ gateway_available: false, reason: '   ' })
    ).toMatchObject({ reason: NO_PROVIDER_REASON })
  })

  test('refuses an off-console setup path rather than rendering it as a link', () => {
    for (const setup_path of [
      'https://evil.example/ai',
      '//evil.example/ai',
      'javascript:alert(1)',
    ]) {
      expect(
        assistantReadiness({ gateway_available: false, setup_path })
      ).toMatchObject({ setupPath: OPENROUTER_SETUP_PATH })
    }
  })

  test('the default reason names the gap in the required words', () => {
    expect(NO_PROVIDER_REASON).toContain('No AI provider is configured')
  })
})

describe('setupPathFromProblem', () => {
  test('reads a console path from a 409 problem document', () => {
    expect(
      setupPathFromProblem({ status: 409, setup_path: '/ai-gateway?add=x' })
    ).toBe('/ai-gateway?add=x')
  })

  test('ignores missing, malformed and external paths', () => {
    expect(setupPathFromProblem(undefined)).toBeUndefined()
    expect(setupPathFromProblem('nope')).toBeUndefined()
    expect(setupPathFromProblem({ setup_path: 3 })).toBeUndefined()
    expect(
      setupPathFromProblem({ setup_path: 'https://evil.example' })
    ).toBeUndefined()
  })
})

describe('isAssistantConversation', () => {
  test('matches only the assistant context type', () => {
    expect(isAssistantConversation({ context_type: 'assistant' })).toBe(true)
    expect(isAssistantConversation({ context_type: 'global' })).toBe(false)
    expect(isAssistantConversation({ context_type: 'project' })).toBe(false)
  })
})

describe('example prompts', () => {
  test('lead with an app Temps builds and deploys from nothing', () => {
    expect(ASSISTANT_EXAMPLE_PROMPTS[0].toLowerCase()).toContain('calculator')
    for (const prompt of ASSISTANT_EXAMPLE_PROMPTS) {
      expect(prompt.toLowerCase()).toContain('deploy')
    }
  })

  test('assume nothing a new user does not have yet', () => {
    const all = ASSISTANT_EXAMPLE_PROMPTS.join('\n').toLowerCase()
    expect(all).not.toContain('my app')
    expect(all).not.toContain('my databases')
    expect(all).not.toContain('last deployment')
  })

  test('are concrete, non-empty questions', () => {
    expect(ASSISTANT_EXAMPLE_PROMPTS.length).toBeGreaterThan(0)
    for (const prompt of ASSISTANT_EXAMPLE_PROMPTS) {
      expect(prompt.trim().length).toBeGreaterThan(10)
    }
  })
})

describe('ask before changes', () => {
  test('only the confirming mode asks', () => {
    expect(asksBeforeChanges(ASK_BEFORE_CHANGES_MODE)).toBe(true)
    expect(asksBeforeChanges(RUN_CHANGES_AUTOMATICALLY_MODE)).toBe(false)
    expect(asksBeforeChanges(null)).toBe(false)
  })

  test('round-trips through the permission mode ids the server accepts', () => {
    expect(permissionModeForAsk(true)).toBe('confirm-actions')
    expect(permissionModeForAsk(false)).toBe('auto')
    expect(asksBeforeChanges(permissionModeForAsk(true))).toBe(true)
    expect(asksBeforeChanges(permissionModeForAsk(false))).toBe(false)
  })
})

describe('last assistant model', () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'window')

  // Defined rather than assigned: another test file in the same run can leave
  // `window` read-only, and plain assignment then throws.
  const setWindow = (value: unknown) =>
    Object.defineProperty(globalThis, 'window', {
      value,
      configurable: true,
      writable: true,
    })

  afterEach(() => {
    if (original) Object.defineProperty(globalThis, 'window', original)
    else Reflect.deleteProperty(globalThis, 'window')
  })

  function fakeWindow(storage: Record<string, string> | 'throws') {
    const localStorage =
      storage === 'throws'
        ? {
            getItem: () => {
              throw new Error('denied')
            },
            setItem: () => {
              throw new Error('denied')
            },
          }
        : {
            getItem: (key: string) => storage[key] ?? null,
            setItem: (key: string, value: string) => {
              storage[key] = value
            },
          }
    setWindow({ localStorage })
  }

  test('remembers the model a chat was last given', () => {
    fakeWindow({})
    expect(loadLastAssistantModel()).toBeNull()
    saveLastAssistantModel('vendor/model-a')
    expect(loadLastAssistantModel()).toBe('vendor/model-a')
  })

  test('falls back quietly when storage is unavailable', () => {
    fakeWindow('throws')
    expect(loadLastAssistantModel()).toBeNull()
    expect(() => saveLastAssistantModel('vendor/model-a')).not.toThrow()
  })

  test('remembers the provider key a chat was last started on', () => {
    fakeWindow({})
    expect(loadLastAssistantKeyId()).toBeNull()
    saveLastAssistantKeyId(7)
    expect(loadLastAssistantKeyId()).toBe(7)
  })

  test('ignores a stored key id that is not a positive integer', () => {
    fakeWindow({ 'temps.assistant.last-key': 'abc' })
    expect(loadLastAssistantKeyId()).toBeNull()
  })

  test('falls back quietly for the key when storage is unavailable', () => {
    fakeWindow('throws')
    expect(loadLastAssistantKeyId()).toBeNull()
    expect(() => saveLastAssistantKeyId(7)).not.toThrow()
  })
})

describe('chatTint', () => {
  test('is stable for the same chat and always yields classes', () => {
    const first = chatTint('conv-a1b2')
    expect(chatTint('conv-a1b2')).toEqual(first)
    expect(first.tile).toContain('bg-')
    expect(first.label).toContain('text-')
  })

  test('spreads different chats over more than one colour', () => {
    const tiles = new Set(
      Array.from({ length: 24 }, (_, i) => chatTint(`chat-${i}`).tile)
    )
    expect(tiles.size).toBeGreaterThan(1)
  })

  test('handles an empty id', () => {
    expect(chatTint('').tile).toContain('bg-')
  })
})
