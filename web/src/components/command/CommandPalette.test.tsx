// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { Command, CommandList } from '@/components/ui/command'
import Fuse from 'fuse.js'

import { CLUSTER_DNS_URL } from '@/lib/worker-nodes'
import { PROJECT_NAV_GROUPS } from '@/lib/project-navigation'
import {
  CommandPaletteSuggestions,
  indexedMainNavItems,
  projectNavItems,
  projectNavItemsFor,
} from './CommandPalette'
import { INTERNAL_ACCESS_SECTION_ID } from '@/components/project/settings/internal-access'
import {
  platformSettingNavigationItems,
  settingsPageNavigationItems,
} from '@/lib/command-navigation-catalog'
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
  test('branding searches open the platform response privacy setting', () => {
    const fuse = new Fuse(
      [...settingsPageNavigationItems, ...platformSettingNavigationItems],
      NAV_FUSE_OPTIONS
    )
    for (const query of ['branding', 'response privacy', 'fingerprinting']) {
      const [first] = fuse.search(query)
      expect(first?.item.url).toBe(
        '/settings/security#setting-response-branding'
      )
    }
    expect(
      projectNavItems.some((item) =>
        item.url.includes('setting-response-branding')
      )
    ).toBe(false)
  })

  test('typing "Cluster DNS" finds the Cluster DNS card first', () => {
    const fuse = new Fuse(indexedMainNavItems, NAV_FUSE_OPTIONS)
    const [first] = fuse.search('Cluster DNS')
    expect(first?.item.title).toBe('Cluster DNS')
    expect(first?.item.url).toBe(CLUSTER_DNS_URL)
  })

  test('the setup checklist stays reachable after its sidebar card is hidden', () => {
    const fuse = new Fuse(indexedMainNavItems, NAV_FUSE_OPTIONS)
    for (const query of [
      'Finish setting up',
      'platform setup',
      'setup checklist',
    ]) {
      const [first] = fuse.search(query)
      expect(first?.item.url).toBe('/setup')
    }
  })

  test('the card is also found by what it does', () => {
    const fuse = new Fuse(indexedMainNavItems, NAV_FUSE_OPTIONS)
    const urls = fuse.search('temps.local').map((result) => result.item.url)
    expect(urls).toContain(CLUSTER_DNS_URL)
  })
})

describe('project pages in the palette', () => {
  test('the Monitoring pages carry the icon of their tab', () => {
    const tabs = PROJECT_NAV_GROUPS.flatMap((group) => group.sections)
      .find((section) => section.id === 'monitoring')
      ?.tabs.filter((tab) =>
        ['monitors', 'cron-jobs', 'check-ins'].includes(tab.url)
      )
    expect(tabs?.map((tab) => tab.title)).toEqual([
      'Uptime',
      'Cron jobs',
      'Check-ins',
    ])
    for (const tab of tabs ?? []) {
      const entry = projectNavItems.find((item) => item.url === tab.url)
      expect({ url: tab.url, icon: entry?.icon }).toEqual({
        url: tab.url,
        icon: tab.icon,
      })
    }
  })

  test('a project finds its Internal access card by name and by temps.local', () => {
    const items = projectNavItemsFor({ source_type: 'git', preset: 'nextjs' })
    const fuse = new Fuse(items, NAV_FUSE_OPTIONS)
    for (const query of ['internal access', 'temps.local']) {
      const [first] = fuse.search(query)
      expect({ query, title: first?.item.title, url: first?.item.url }).toEqual(
        {
          query,
          title: 'Internal access',
          url: `settings/security#${INTERNAL_ACCESS_SECTION_ID}`,
        }
      )
    }
  })
})
