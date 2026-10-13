// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { ProviderKeyResponse } from '@/api/client'
import { SelfHostedKeySettings } from './SelfHostedKeySettings'

const baseKey: ProviderKeyResponse = {
  id: 7,
  provider: 'openai_compatible',
  display_name: 'Local MLX server',
  api_key_masked: '***',
  base_url: 'http://10.0.0.5:8000/v1',
  default_model: 'mlx-community/Some-Model-4bit',
  is_active: true,
  trusted_private_address: '10.0.0.5:8000',
  trusted_private_address_set_at: '2026-10-11T22:00:00Z',
  capabilities: {
    streaming: true,
    tools: true,
    embeddings: true,
    responses_api: false,
  },
  created_at: '2026-10-11T22:00:00Z',
  updated_at: '2026-10-11T22:00:00Z',
}

function render(key: ProviderKeyResponse) {
  return renderToStaticMarkup(
    <SelfHostedKeySettings
      providerKey={key}
      canTrustPrivateAddress
      onUpdate={() => {}}
      pending={false}
    />
  )
}

test('shows the trusted address, base URL and declared capabilities', () => {
  const html = render(baseKey)
  expect(html).toContain('Trusts 10.0.0.5:8000')
  expect(html).toContain('Remove trust')
  expect(html).toContain('http://10.0.0.5:8000/v1')
  expect(html).toContain('Embeddings')
})

test('offers trusting an IP-literal base URL and stays empty for plain vendor keys', () => {
  expect(render({ ...baseKey, trusted_private_address: null })).toContain(
    'Trust this address for this key'
  )
  expect(
    render({
      ...baseKey,
      provider: 'openai',
      trusted_private_address: null,
      base_url: null,
    })
  ).toBe('')
})
