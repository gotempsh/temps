// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  GROUP_NAV_GROUPS,
  GROUP_NAV_SECTIONS,
  SERVER_WIDE_LISTS,
  groupSectionDestinations,
  groupSectionHref,
  legacyGroupTabRedirect,
  resolveConsoleScope,
  resolveGroupSection,
} from './console-scope'
import { primaryPlatformGroups } from '@/components/dashboard/primary-navigation'
import { platformToolGroups } from '@/components/platform/platform-tools'

describe('resolveConsoleScope', () => {
  test('a project (group) and everything below it is project scope', () => {
    expect(resolveConsoleScope('/p/storefront')).toEqual({
      kind: 'project',
      groupSlug: 'storefront',
      route: '',
    })
    expect(resolveConsoleScope('/p/storefront/databases/')).toEqual({
      kind: 'project',
      groupSlug: 'storefront',
      route: 'databases',
    })
  })

  test('a service and its pages are service scope', () => {
    expect(resolveConsoleScope('/projects/web-app/errors/42')).toEqual({
      kind: 'service',
      serviceSlug: 'web-app',
      route: 'errors/42',
    })
    expect(resolveConsoleScope('/projects/web-app')).toEqual({
      kind: 'service',
      serviceSlug: 'web-app',
      route: '',
    })
  })

  test('the project list and creation pages stay global', () => {
    for (const path of [
      '/projects',
      '/projects/new',
      '/projects/import-wizard',
      '/projects/import/12',
      '/storage',
      '/settings/nodes',
      '/',
    ]) {
      expect(resolveConsoleScope(path)).toEqual({ kind: 'global' })
    }
  })
})

describe('project navigation', () => {
  const ids = GROUP_NAV_SECTIONS.map((section) => section.id)

  test('holds the project pages operators asked for', () => {
    expect(ids).toEqual([
      'overview',
      'deployments',
      'logs',
      'errors',
      'traces',
      'analytics',
      'domains',
      'databases',
      'backups',
      'variables',
      'settings',
    ])
  })

  test('holds no server-wide administration', () => {
    const urls = GROUP_NAV_SECTIONS.map((section) =>
      groupSectionHref('example', section)
    )
    for (const url of urls) expect(url.startsWith('/p/example')).toBe(true)
    const titles = GROUP_NAV_SECTIONS.map((section) => section.title)
    for (const admin of ['Worker nodes', 'Users', 'Plugins', 'All tools'])
      expect(titles).not.toContain(admin)
  })

  test('groups sections by job, Overview first and unlabelled', () => {
    expect(GROUP_NAV_GROUPS[0].label).toBeUndefined()
    expect(GROUP_NAV_GROUPS.map((group) => group.label)).toEqual([
      undefined,
      'Observe',
      'Resources',
      'Configure',
    ])
  })

  test('resolves sub-paths to their section', () => {
    expect(resolveGroupSection('')).toBe('overview')
    expect(resolveGroupSection('databases')).toBe('databases')
    expect(resolveGroupSection('/logs/')).toBe('logs')
    expect(resolveGroupSection('repository')).toBe('settings')
    expect(resolveGroupSection('domainsx')).toBe('overview')
  })

  test('builds encoded section URLs', () => {
    const databases = GROUP_NAV_SECTIONS.find((s) => s.id === 'databases')!
    const overview = GROUP_NAV_SECTIONS.find((s) => s.id === 'overview')!
    expect(groupSectionHref('a b', databases)).toBe('/p/a%20b/databases')
    expect(groupSectionHref('shop', overview)).toBe('/p/shop')
  })
})

describe('nothing becomes unreachable', () => {
  const globalUrls = new Set([
    ...primaryPlatformGroups.flatMap((group) =>
      group.items.map((item) => item.url)
    ),
    ...platformToolGroups.flatMap((group) =>
      group.items.map((item) => item.url)
    ),
  ])

  test('every server-wide list a project page links to is in the global navigation', () => {
    for (const list of Object.values(SERVER_WIDE_LISTS))
      expect(globalUrls.has(list.url)).toBe(true)
  })

  test('every project page is a command palette destination', () => {
    const destinations = groupSectionDestinations({
      id: 3,
      name: 'Storefront',
      slug: 'storefront',
    })
    const urls = destinations.map((d) => d.url)
    for (const section of GROUP_NAV_SECTIONS.filter((s) => s.url))
      expect(urls).toContain(groupSectionHref('storefront', section))
    expect(new Set(destinations.map((d) => d.id)).size).toBe(
      destinations.length
    )
    expect(
      destinations.find((d) => d.url === '/p/storefront/databases')
    ).toMatchObject({
      title: 'Storefront · Databases',
      keywords: expect.arrayContaining(['storefront', 'Databases']),
    })
  })
})

describe('legacyGroupTabRedirect', () => {
  test('moves the former overview tabs to their pages', () => {
    expect(legacyGroupTabRedirect('shop', '?tab=variables')).toBe(
      '/p/shop/variables'
    )
    expect(legacyGroupTabRedirect('shop', '?tab=settings&section=routes')).toBe(
      '/p/shop/settings?section=routes'
    )
  })

  test('drops an unknown or overview tab, keeping other parameters', () => {
    expect(legacyGroupTabRedirect('shop', '?tab=overview')).toBe('/p/shop')
    expect(legacyGroupTabRedirect('shop', '?tab=nope&import=1')).toBe(
      '/p/shop?import=1'
    )
  })

  test('keeps the hash, so a deep link still lands on its section', () => {
    expect(
      legacyGroupTabRedirect(
        'shop',
        '?tab=settings&section=routes',
        '#setting-routes'
      )
    ).toBe('/p/shop/settings?section=routes#setting-routes')
    expect(legacyGroupTabRedirect('shop', '?tab=variables', '#api')).toBe(
      '/p/shop/variables#api'
    )
    expect(legacyGroupTabRedirect('shop', '?tab=variables', '')).toBe(
      '/p/shop/variables'
    )
  })

  test('leaves a URL without a tab alone', () => {
    expect(legacyGroupTabRedirect('shop', '')).toBeNull()
    expect(legacyGroupTabRedirect('shop', '?import=1')).toBeNull()
    expect(legacyGroupTabRedirect('shop', '', '#routes')).toBeNull()
  })
})
