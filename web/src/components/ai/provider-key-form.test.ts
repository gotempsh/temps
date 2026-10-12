// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  canSubmitProviderKeyForm,
  defaultProviderKeyDisplayName,
  EMPTY_PROVIDER_KEY_FORM,
  providerKeyFormForProvider,
  providerKeyFormSchema,
  selectProviderKeyFormProvider,
  providerKeyFormToRequest,
  type ProviderKeyFormValues,
} from './provider-key-form'
import { selfHostedDiscoverySignature } from './self-hosted-provider'

const MODEL = 'mlx-community/Some-Model-4bit'

function selfHosted(
  overrides: Partial<ProviderKeyFormValues> = {}
): ProviderKeyFormValues {
  const values: ProviderKeyFormValues = {
    ...EMPTY_PROVIDER_KEY_FORM,
    provider: 'openai_compatible',
    display_name: 'Local MLX server',
    base_url: 'http://10.0.0.5:8000/v1',
    trust_private_address: true,
    default_model: MODEL,
    ...overrides,
  }
  if ('discovery' in overrides) return values
  return {
    ...values,
    discovery: {
      signature: selfHostedDiscoverySignature({
        baseUrl: values.base_url,
        apiKey: values.api_key,
        trustPrivateAddress: values.trust_private_address,
      }),
      models: [MODEL],
    },
  }
}

function issues(values: ProviderKeyFormValues) {
  const result = providerKeyFormSchema.safeParse(values)
  return result.success
    ? []
    : result.error.issues.map(
        (issue) => `${issue.path.join('.')}: ${issue.message}`
      )
}

describe('provider key form', () => {
  test('vendor providers need a display name and an API key', () => {
    expect(issues({ ...EMPTY_PROVIDER_KEY_FORM, provider: 'openai' })).toEqual([
      'display_name: Please enter a display name',
      'api_key: Please enter an API key',
    ])
    expect(
      issues({
        ...EMPTY_PROVIDER_KEY_FORM,
        provider: 'openai',
        display_name: 'OpenAI',
        api_key: 'sk-x',
      })
    ).toEqual([])
  })

  test('a self-hosted provider needs a discovered default model, not an API key', () => {
    expect(issues(selfHosted())).toEqual([])
    expect(issues(selfHosted({ base_url: '' }))[0]).toContain('base_url')
    expect(issues(selfHosted({ discovery: null }))[0]).toContain(
      'Load the models'
    )
    expect(issues(selfHosted({ default_model: 'gpt-5-nano' }))[0]).toContain(
      'Pick a default model'
    )
  })

  test('a discovery made for other inputs no longer counts', () => {
    const values = selfHosted()
    expect(
      issues({ ...values, base_url: 'http://10.0.0.6:8000/v1' })[0]
    ).toContain('Load the models')
  })

  test('only self-hosted requests carry the self-hosted fields', () => {
    expect(providerKeyFormToRequest(selfHosted())).toEqual({
      provider: 'openai_compatible',
      display_name: 'Local MLX server',
      api_key: '',
      base_url: 'http://10.0.0.5:8000/v1',
      default_model: MODEL,
      trust_private_address: true,
      capabilities: EMPTY_PROVIDER_KEY_FORM.capabilities,
    })
    expect(
      providerKeyFormToRequest({
        ...EMPTY_PROVIDER_KEY_FORM,
        provider: 'openai',
        display_name: ' OpenAI ',
        api_key: 'sk-x',
        trust_private_address: true,
      })
    ).toEqual({
      provider: 'openai',
      display_name: 'OpenAI',
      api_key: 'sk-x',
      base_url: undefined,
    })
  })
})

describe('provider choice inside the add-key dialog', () => {
  const openai = { id: 'openai', name: 'OpenAI' }
  const anthropic = { id: 'anthropic', name: 'Anthropic' }
  const names = ['OpenAI', 'Anthropic', 'Self-hosted (OpenAI-compatible)']

  test('submit stays disabled until a provider is chosen', () => {
    expect(canSubmitProviderKeyForm(EMPTY_PROVIDER_KEY_FORM)).toBe(false)
    expect(canSubmitProviderKeyForm({ provider: '  ' })).toBe(false)
    expect(canSubmitProviderKeyForm({ provider: 'openai' })).toBe(true)
  })

  test('a form without a provider fails validation with the provider message', () => {
    const found = issues({
      ...EMPTY_PROVIDER_KEY_FORM,
      display_name: 'Production',
      api_key: 'sk-test',
    })
    expect(found).toContain('provider: Please select a provider')
  })

  test('numbers display names only once the provider already has keys', () => {
    expect(defaultProviderKeyDisplayName('OpenAI', 0)).toBe('OpenAI')
    expect(defaultProviderKeyDisplayName('OpenAI', 2)).toBe('OpenAI (key 3)')
  })

  test('the preselected flow fixes the provider and pre-fills the name', () => {
    expect(providerKeyFormForProvider(openai, 1)).toEqual({
      ...EMPTY_PROVIDER_KEY_FORM,
      provider: 'openai',
      display_name: 'OpenAI (key 2)',
    })
  })

  test('choosing a provider pre-fills an empty display name', () => {
    const next = selectProviderKeyFormProvider(
      EMPTY_PROVIDER_KEY_FORM,
      anthropic,
      0,
      names
    )
    expect(next.provider).toBe('anthropic')
    expect(next.display_name).toBe('Anthropic')
  })

  test('switching provider follows a pre-filled name but keeps a typed one', () => {
    const first = selectProviderKeyFormProvider(
      EMPTY_PROVIDER_KEY_FORM,
      openai,
      0,
      names
    )
    const switched = selectProviderKeyFormProvider(first, anthropic, 1, names)
    expect(switched.display_name).toBe('Anthropic (key 2)')

    const typed = selectProviderKeyFormProvider(
      { ...first, display_name: 'Production key' },
      anthropic,
      0,
      names
    )
    expect(typed.display_name).toBe('Production key')
  })

  test('switching provider drops fields that belong to the previous one', () => {
    const selfHostedForm = selfHosted({ api_key: 'secret' })
    const next = selectProviderKeyFormProvider(selfHostedForm, openai, 0, names)
    expect(next.provider).toBe('openai')
    expect(next.api_key).toBe('')
    expect(next.base_url).toBe('')
    expect(next.default_model).toBe('')
    expect(next.trust_private_address).toBe(false)
    expect(next.discovery).toBeNull()
  })

  test('re-choosing the current provider changes nothing', () => {
    const current = {
      ...providerKeyFormForProvider(openai, 0),
      api_key: 'sk-x',
    }
    expect(selectProviderKeyFormProvider(current, openai, 0, names)).toBe(
      current
    )
  })
})
