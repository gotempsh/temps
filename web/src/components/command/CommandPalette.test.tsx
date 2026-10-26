// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { Command, CommandList } from '@/components/ui/command'
import Fuse from 'fuse.js'

import { CLUSTER_DNS_URL } from '@/lib/worker-nodes'
import {
  CommandPaletteSuggestions,
  indexedMainNavItems,
} from './CommandPalette'
import { NAV_FUSE_OPTIONS } from './nav-search'

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

describe('navigation search', () => {
  test('typing "Cluster DNS" finds the Cluster DNS card first', () => {
    const fuse = new Fuse(indexedMainNavItems, NAV_FUSE_OPTIONS)
    const [first] = fuse.search('Cluster DNS')
    expect(first?.item.title).toBe('Cluster DNS')
    expect(first?.item.url).toBe(CLUSTER_DNS_URL)
  })

  test('the card is also found by what it does', () => {
    const fuse = new Fuse(indexedMainNavItems, NAV_FUSE_OPTIONS)
    const urls = fuse.search('temps.local').map((result) => result.item.url)
    expect(urls).toContain(CLUSTER_DNS_URL)
  })
})
