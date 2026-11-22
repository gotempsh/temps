// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { AiAssistantButton } from './AiAssistantButton'
import { AiAssistantProvider } from './AiAssistantContext'

describe('AiAssistantButton', () => {
  test('always renders the global launcher without requiring a gateway key', () => {
    const markup = renderToStaticMarkup(
      <AiAssistantProvider>
        <MemoryRouter>
          <AiAssistantButton />
        </MemoryRouter>
      </AiAssistantProvider>
    )

    expect(markup).toContain('title="AI assistant"')
    expect(markup).toContain('AI assistant</span>')
    expect(markup).toContain('aria-pressed="false"')
  })

  test('shows as pressed while the full-screen assistant is open', () => {
    const markup = renderToStaticMarkup(
      <AiAssistantProvider>
        <MemoryRouter initialEntries={['/chat']}>
          <AiAssistantButton />
        </MemoryRouter>
      </AiAssistantProvider>
    )

    expect(markup).toContain('aria-pressed="true"')
  })
})
