// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  activeAssistantKeys,
  assistantKeyLabel,
  assistantRuntimeSelection,
  defaultAssistantKey,
  gatewayKeyIdFromProvider,
  gatewayKeyProviderId,
  resolveAssistantKey,
  type AssistantKey,
} from './assistant-keys'
import type {
  ChatProviderOption,
  ChatRuntimeSelection,
} from './chat-runtime-options'

const openai: AssistantKey = {
  id: 1,
  provider: 'openai',
  display_name: 'OpenAI',
  is_active: true,
}
const compatible: AssistantKey = {
  id: 2,
  provider: 'openai_compatible',
  display_name: 'Hosted models',
  is_active: true,
  base_url: 'https://api.example.com/v1',
}
const openrouter: AssistantKey = {
  id: 3,
  provider: 'openrouter',
  display_name: 'OpenRouter',
  is_active: true,
}
const disabled: AssistantKey = {
  id: 4,
  provider: 'anthropic',
  display_name: 'Anthropic',
  is_active: false,
}

describe('the default assistant key', () => {
  test('is the active OpenRouter key when there is one', () => {
    expect(defaultAssistantKey([openai, compatible, openrouter])?.id).toBe(3)
  })

  test('is the first active key without OpenRouter', () => {
    expect(defaultAssistantKey([disabled, compatible, openai])?.id).toBe(2)
  })

  test('is null without an active key', () => {
    expect(defaultAssistantKey([disabled])).toBeNull()
    expect(defaultAssistantKey([])).toBeNull()
  })

  test('skips disabled keys when listing choices', () => {
    expect(activeAssistantKeys([openai, disabled]).map((k) => k.id)).toEqual([
      1,
    ])
  })
})

describe('resolving the key a new chat starts on', () => {
  test('keeps the preferred key while it is active', () => {
    expect(resolveAssistantKey([openai, compatible, openrouter], 2)?.id).toBe(2)
  })

  test('falls back to the default for a disabled or deleted key', () => {
    expect(resolveAssistantKey([openai, openrouter, disabled], 4)?.id).toBe(3)
    expect(resolveAssistantKey([openai, openrouter], 99)?.id).toBe(3)
    expect(resolveAssistantKey([openai, openrouter], null)?.id).toBe(3)
  })
})

describe('gateway key provider ids', () => {
  test('round-trip a key id', () => {
    expect(gatewayKeyProviderId(7)).toBe('gateway_key:7')
    expect(gatewayKeyIdFromProvider('gateway_key:7')).toBe(7)
  })

  test('are null for other providers and malformed ids', () => {
    expect(gatewayKeyIdFromProvider('gateway')).toBeNull()
    expect(gatewayKeyIdFromProvider('claude-code')).toBeNull()
    expect(gatewayKeyIdFromProvider('gateway_key:')).toBeNull()
    expect(gatewayKeyIdFromProvider('gateway_key:abc')).toBeNull()
    expect(gatewayKeyIdFromProvider(null)).toBeNull()
  })
})

describe('key labels', () => {
  test('name a vendor key by its display name', () => {
    expect(assistantKeyLabel(openai)).toBe('OpenAI')
  })

  test('add the endpoint host for a custom base URL', () => {
    expect(assistantKeyLabel(compatible)).toBe(
      'Hosted models · api.example.com'
    )
  })

  test('fall back to the provider name for a blank display name', () => {
    expect(assistantKeyLabel({ ...openrouter, display_name: '  ' })).toBe(
      'OpenRouter'
    )
  })
})

describe('moving an assistant chat to another key', () => {
  const keyProvider = (id: number, models: string[]): ChatProviderOption => ({
    id: gatewayKeyProviderId(id),
    name: `Key ${id}`,
    auth_source: 'gateway',
    models: models.map((model) => ({
      id: model,
      name: model,
      thinking_options: [],
    })),
    default_model_id: models[0],
    model_source: 'live',
    permission_modes: [{ id: 'confirm-actions', name: 'Ask first' }],
    default_permission_mode_id: 'confirm-actions',
  })
  const providers = [
    keyProvider(1, ['model-a']),
    keyProvider(2, ['model-b', 'model-c']),
  ]
  const current: ChatRuntimeSelection = {
    providerId: gatewayKeyProviderId(1),
    modelId: 'model-a',
    thinkingOptionId: null,
    permissionModeId: 'confirm-actions',
  }

  test('runs on the chosen key with the chosen model', () => {
    expect(
      assistantRuntimeSelection(
        providers,
        current,
        gatewayKeyProviderId(2),
        'model-c'
      )
    ).toMatchObject({
      providerId: 'gateway_key:2',
      modelId: 'model-c',
      permissionModeId: 'confirm-actions',
    })
  })

  test('a key the status has not listed yet is kept, not swapped for another', () => {
    expect(
      assistantRuntimeSelection(
        providers,
        current,
        gatewayKeyProviderId(7),
        'model-z'
      )
    ).toEqual({
      providerId: 'gateway_key:7',
      modelId: 'model-z',
      thinkingOptionId: null,
      permissionModeId: 'confirm-actions',
    })
  })
})
