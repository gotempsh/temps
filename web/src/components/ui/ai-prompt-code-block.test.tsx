// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AiPromptCodeBlock, CopyAiPromptButton } from './ai-prompt-code-block'

test('AI prompt action is rendered without provider configuration or hover', () => {
  const html = renderToStaticMarkup(
    <CopyAiPromptButton prompt="Integrate KV" />
  )
  expect(html).toContain('aria-label="Copy AI prompt"')
  expect(html).toContain('wand-sparkles')
  expect(html).not.toContain('opacity-0')
})
test('examples retain code copy alongside the separate AI prompt action', () => {
  const html = renderToStaticMarkup(
    <AiPromptCodeBlock
      code="const value = 1"
      language="typescript"
      prompt="Adapt this example"
    />
  )
  expect(html).toContain('Copy AI prompt')
  expect(html).toContain('aria-label="Copy"')
  expect(html).toContain('const value = 1')
})
