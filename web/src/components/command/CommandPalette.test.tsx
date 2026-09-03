// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { Command, CommandList } from '@/components/ui/command'
import { CommandPaletteSuggestions } from './CommandPalette'

describe('CommandPaletteSuggestions', () => {
  test('renders natural-language prompts as compact command rows', () => {
    const markup = renderToStaticMarkup(
      <Command>
        <CommandList>
          <CommandPaletteSuggestions
            queries={['Show me all projects', 'Open platform monitoring']}
            onSelect={() => undefined}
          />
        </CommandList>
      </Command>
    )

    expect(markup).toContain('Suggested searches')
    expect(markup).toContain('Show me all projects')
    expect(markup).toContain('gap-3 py-2.5')
    expect(markup).not.toContain('uppercase')
    expect(markup).not.toContain('min-h-20')
  })
})
