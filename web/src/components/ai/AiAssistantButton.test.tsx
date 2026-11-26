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

    // Named as the sidebar names it, with the same icon.
    expect(markup).toContain('lucide-sparkles')
    expect(markup).toContain('Ask</button>')
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

  test('stands in the top bar at every width', () => {
    const markup = renderToStaticMarkup(
      <AiAssistantProvider>
        <MemoryRouter>
          <AiAssistantButton />
        </MemoryRouter>
      </AiAssistantProvider>
    )

    expect(markup).not.toMatch(/(^|[\s"])(max-)?(sm|md|lg):hidden/)
  })
})
