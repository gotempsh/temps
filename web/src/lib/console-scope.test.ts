// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  GROUP_NAV_SECTIONS,
  SERVER_WIDE_LISTS,
  groupSectionDestinations,
  groupPathHref,
  groupSectionHref,
  resolveGroupLocation,
  legacyGroupTabRedirect,
  resolveConsoleScope,
  resolveGroupSection,
} from './console-scope'
import { SERVER_AREAS } from './server-navigation'

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
      'variables',
      'settings',
    ])
  })

  test('Backups is a page of Databases, not an entry of its own', () => {
    const databases = GROUP_NAV_SECTIONS.find((s) => s.id === 'databases')
    expect(databases?.tabs?.map((tab) => [tab.title, tab.url])).toEqual([
      ['Databases', 'databases'],
      ['Backups', 'backups'],
    ])
    expect(resolveGroupSection('backups')).toBe('databases')
    expect(resolveGroupLocation('backups').tab?.title).toBe('Backups')
    expect(resolveGroupLocation('databases').tab?.title).toBe('Databases')
    // A record below a tab is no tab's own page.
    expect(resolveGroupLocation('backups/3').tab).toBeUndefined()
    expect(resolveGroupLocation('backups/3').section?.id).toBe('databases')
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

  test('is one list of views, with no parent grouping them by kind', () => {
    expect(GROUP_NAV_SECTIONS.map((section) => section.title)).toEqual([
      'Overview',
      'Deployments',
      'Logs',
      'Errors',
      'Traces',
      'Analytics',
      'Domains',
      'Databases',
      'Shared variables',
      'Settings',
    ])
    // The former parents are not pages: their sub-paths are the overview's.
    for (const path of ['observe', 'resources', 'configure'])
      expect(resolveGroupLocation(path).section.id).toBe('overview')
  })

  test('a sub-path is a view, and the tab of it when the view has several', () => {
    expect(resolveGroupLocation('').section.id).toBe('overview')
    expect(resolveGroupLocation('logs')).toMatchObject({
      section: { id: 'logs' },
      tab: undefined,
    })
    expect(resolveGroupLocation('backups')).toMatchObject({
      section: { id: 'databases' },
      tab: { title: 'Backups' },
    })
    expect(groupPathHref('a b', 'backups')).toBe('/p/a%20b/backups')
  })

  test('a view that holds nothing until a service uses it says what is missing', () => {
    const missing = GROUP_NAV_SECTIONS.filter((section) => section.missing)
    expect(missing.map((section) => section.id)).toEqual([
      'traces',
      'analytics',
      'domains',
      'databases',
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
  const globalUrls = new Set(
    SERVER_AREAS.flatMap((area) => [
      area.url,
      ...(area.tabs ?? []).map((tab) => tab.url),
    ])
  )

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
    // A view's second page is a page too; the former parents are not.
    expect(urls).not.toContain('/p/storefront/resources')
    expect(
      destinations.find((d) => d.url === '/p/storefront/backups')?.title
    ).toBe('Storefront · Databases · Backups')
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
