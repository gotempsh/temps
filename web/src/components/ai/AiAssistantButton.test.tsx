// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { AiAssistantButton } from './AiAssistantButton'
import { AiAssistantProvider } from './AiAssistantContext'

describe('AiAssistantButton', () => {
  test('always renders the global launcher without requiring a gateway key', () => {
    const markup = renderToStaticMarkup(
      <AiAssistantProvider>
        <AiAssistantButton />
      </AiAssistantProvider>
    )

    expect(markup).toContain('title="AI assistant"')
    expect(markup).toContain('AI assistant</span>')
  })
})
