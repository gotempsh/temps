// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AssistantKeySelect } from './AssistantModelPicker'

const keys = [
  {
    id: 3,
    provider: 'openrouter',
    display_name: 'OpenRouter',
    is_active: true,
  },
  {
    id: 5,
    provider: 'openai_compatible',
    display_name: 'Hosted models',
    is_active: true,
    base_url: 'https://api.example.com/v1',
  },
]

test('offers the provider when more than one key is active', () => {
  const markup = renderToStaticMarkup(
    <AssistantKeySelect keys={keys} value={3} onChange={() => {}} />
  )
  expect(markup).toContain('Provider')
  expect(markup).toContain('aria-label="Provider for this chat"')
  expect(markup).toContain('role="combobox"')
})

test('a single key needs no provider choice', () => {
  const markup = renderToStaticMarkup(
    <AssistantKeySelect keys={keys.slice(0, 1)} value={3} onChange={() => {}} />
  )
  expect(markup).toBe('')
})
