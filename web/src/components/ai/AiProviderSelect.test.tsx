// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AiProviderLabel } from './AiProviderSelect'
import { AI_PROVIDERS } from '@/lib/ai-providers'
const GENAI_PROVIDERS = [
  ...AI_PROVIDERS,
  { id: 'mistral', name: 'Mistral' },
  { id: 'deepseek', name: 'DeepSeek' },
]

test('every GenAI provider has a labelled brand mark', () => {
  for (const provider of GENAI_PROVIDERS) {
    const html = renderToStaticMarkup(
      <AiProviderLabel provider={provider.id} />
    )
    expect(html).toContain(provider.name)
    expect(html).toMatch(/<(svg|img) /)
  }
})
test('additional telemetry providers use their own assets in both themes', () => {
  for (const provider of ['mistral', 'deepseek', 'openrouter']) {
    const html = renderToStaticMarkup(<AiProviderLabel provider={provider} />)
    expect(html).toContain(`/ai-agents/${provider}.svg`)
    expect(html).toContain('dark:invert')
  }
})
test('unknown provider identities remain readable', () => {
  expect(
    renderToStaticMarkup(<AiProviderLabel provider="custom-provider" />)
  ).toContain('custom-provider')
})
