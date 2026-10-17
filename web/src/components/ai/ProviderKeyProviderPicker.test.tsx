// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { AI_PROVIDERS } from '@/lib/ai-providers'
import { ProviderKeyProviderPicker } from './ProviderKeyProviderPicker'

function render(value: string, disabled = false) {
  return renderToStaticMarkup(
    <ProviderKeyProviderPicker
      providers={AI_PROVIDERS}
      value={value}
      onChange={() => {}}
      disabled={disabled}
    />
  )
}

describe('ProviderKeyProviderPicker', () => {
  test('offers every provider the gateway rows list', () => {
    const html = render('')
    for (const provider of AI_PROVIDERS) {
      expect(html).toContain(`data-provider="${provider.id}"`)
      expect(html).toContain(provider.name)
    }
  })

  test('nothing is selected until a provider is chosen', () => {
    const html = render('')
    expect(html).not.toContain('aria-checked="true"')
    expect(html.match(/aria-checked="false"/g)?.length).toBe(
      AI_PROVIDERS.length
    )
  })

  test('marks only the chosen provider as selected', () => {
    const html = render('anthropic')
    expect(html.match(/aria-checked="true"/g)?.length).toBe(1)
    expect(html).toMatch(/aria-checked="true"[^>]*data-provider="anthropic"/)
  })

  test('disables every choice while a save is in flight', () => {
    const html = render('openai', true)
    expect(html.match(/disabled=""/g)?.length).toBe(AI_PROVIDERS.length)
  })
})
