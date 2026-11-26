// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { AI_PROVIDERS } from '@/lib/ai-providers-shared'
import {
  ASSISTANT_PROVIDER_CHOICES,
  PROVIDER_KEY_PLACEHOLDER,
  providerTestRequest,
  providerTestSignature,
} from './assistant-providers'

test('every provider the gateway supports is offered, each once', () => {
  const offered = ASSISTANT_PROVIDER_CHOICES.map((choice) => choice.id)
  expect([...offered].sort()).toEqual(
    AI_PROVIDERS.map((provider) => provider.id).sort()
  )
  expect(new Set(offered).size).toBe(offered.length)
})

test('the key that reaches every vendor comes first, a server of your own last', () => {
  const offered = ASSISTANT_PROVIDER_CHOICES.map((choice) => choice.name)
  expect(offered).toEqual([
    'OpenRouter',
    'OpenAI',
    'Anthropic',
    'Google Gemini',
    'xAI',
    'OpenAI API',
  ])
})

test('a vendor shows how its keys begin; a self-hosted server has no such form', () => {
  for (const choice of ASSISTANT_PROVIDER_CHOICES) {
    const hint = PROVIDER_KEY_PLACEHOLDER[choice.id]
    if (choice.id === 'openai_compatible') expect(hint).toBe('')
    else expect(hint.endsWith('…')).toBe(true)
  }
})

test('a vendor is tested with its key alone', () => {
  expect(
    providerTestRequest({
      provider: 'anthropic',
      api_key: 'sk-ant-example',
      base_url: '',
      default_model: '',
      trust_private_address: false,
    })
  ).toEqual({ provider: 'anthropic', api_key: 'sk-ant-example' })
})

test('a server of your own is tested at its address, on the model picked', () => {
  expect(
    providerTestRequest({
      provider: 'openai_compatible',
      api_key: '',
      base_url: ' http://10.0.0.5:8000/v1 ',
      default_model: 'example-model',
      trust_private_address: true,
    })
  ).toEqual({
    provider: 'openai_compatible',
    api_key: '',
    base_url: 'http://10.0.0.5:8000/v1',
    default_model: 'example-model',
    trust_private_address: true,
  })
})

test('a test result belongs to the inputs it was made with', () => {
  const typed = {
    provider: 'openai',
    api_key: 'sk-one',
    base_url: '',
    default_model: '',
    trust_private_address: false,
  }
  expect(providerTestSignature(typed)).toBe(providerTestSignature({ ...typed }))
  expect(providerTestSignature({ ...typed, api_key: 'sk-two' })).not.toBe(
    providerTestSignature(typed)
  )
  expect(providerTestSignature({ ...typed, provider: 'xai' })).not.toBe(
    providerTestSignature(typed)
  )
})
