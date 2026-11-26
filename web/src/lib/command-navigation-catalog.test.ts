// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { SERVER_AREAS } from '@/lib/server-navigation'
import { settingsNavigationGroups } from '@/components/settings/settings-navigation'
import {
  AUDIT_LOGS_URL,
  buildAccessibleNavigationMap,
  excludeNavigationUrls,
  filterRestrictedNavigationItems,
  mergeNavigationItems,
  serverNavigationItems,
  settingsPageNavigationItems,
} from './command-navigation-catalog'

describe('command navigation catalog', () => {
  test('indexes every server-wide page, set up or not', () => {
    const expectedUrls = SERVER_AREAS.flatMap((area) => [
      area.url,
      ...(area.tabs ?? []).map((tab) => tab.url),
    ])
    const indexedUrls = serverNavigationItems.map((item) => item.url)

    expect(expectedUrls.every((url) => indexedUrls.includes(url))).toBe(true)
    expect(new Set(indexedUrls).size).toBe(indexedUrls.length)
    expect(indexedUrls).toContain('/certificates')
    expect(indexedUrls).not.toContain('/tools')
  })

  test('names a tab with its area, and an area as the menu does', () => {
    const titleOf = (url: string) =>
      serverNavigationItems.find((item) => item.url === url)?.title

    expect(titleOf('/storage')).toBe('Databases')
    expect(titleOf('/backups')).toBe('Databases · Backups')
    expect(titleOf('/proxy-logs')).toBe('Proxy · Requests')
    expect(titleOf('/settings/notifications')).toBe('Alerts · Channels')
    // The former parents were pages of their own; they are gone.
    expect(titleOf('/resources')).toBeUndefined()
    // A page listed in Settings is found by that word too.
    expect(
      serverNavigationItems.find((item) => item.url === '/git-providers')
        ?.keywords
    ).toContain('settings Platform')
    expect(
      serverNavigationItems.find((item) => item.url === '/settings/nodes')
        ?.keywords
    ).toContain('worker nodes')
  })

  test('indexes every page rendered by the settings sidebar', () => {
    const expectedUrls = settingsNavigationGroups.flatMap((group) =>
      group.items.map((item) => item.url)
    )
    const indexedUrls = new Set(
      settingsPageNavigationItems.map((item) => item.url)
    )

    expect(expectedUrls.every((url) => indexedUrls.has(url))).toBe(true)
    // The pages behind the tabs of Limits are destinations of their own.
    for (const url of [
      '/settings/version',
      '/settings/request-timeouts',
      '/settings/service-healthchecks',
      '/settings/traefik-discovery',
      '/settings/otel-pipeline',
      '/settings/plugins',
      AUDIT_LOGS_URL,
    ]) {
      expect(indexedUrls.has(url)).toBe(true)
    }
    expect(
      settingsPageNavigationItems.find(
        (item) => item.url === '/settings/request-timeouts'
      )?.title
    ).toBe('Limits · Request timeouts')
    // An area of the server menu is not also a settings result.
    expect(indexedUrls.has('/settings/mcp-server')).toBe(false)
  })

  test('keeps one result per URL and combines search keywords', () => {
    const [canonical] = settingsPageNavigationItems.filter(
      (item) => item.url === '/settings'
    )
    const results = mergeNavigationItems(
      [
        {
          ...canonical,
          title: 'Platform settings',
          keywords: ['configuration'],
        },
      ],
      [canonical]
    )
    const [result] = results

    expect(results).toHaveLength(1)
    expect(new Set(results.map((item) => item.url)).size).toBe(results.length)
    expect(result.title).toBe('Platform settings')
    expect(result.keywords).toContain('configuration')
    expect(result.keywords).toContain('settings')
    // The name the later list used is still a way to find the page.
    expect(result.keywords).toContain(canonical.title)
  })

  test('keeps settings pages out of the main navigation category', () => {
    const settingsUrls = new Set(
      settingsPageNavigationItems.map((item) => item.url)
    )
    const mainItems = excludeNavigationUrls(
      [
        ...serverNavigationItems,
        { title: 'Platform settings', url: '/settings', icon: () => null },
      ],
      settingsUrls
    )

    expect(mainItems.some((item) => item.url === '/settings')).toBe(false)
    expect(mainItems.some((item) => item.url === '/projects')).toBe(true)
    // Connecting an agent is an AI page, wherever its URL sits.
    expect(mainItems.some((item) => item.url === '/settings/mcp-server')).toBe(
      true
    )
  })

  test('removes restricted audit navigation for users without access', () => {
    const items = [
      { title: 'Proxy logs', url: '/proxy-logs' },
      { title: 'Audit logs', url: AUDIT_LOGS_URL },
    ]

    expect(filterRestrictedNavigationItems(items, false)).toEqual([items[0]])
    expect(filterRestrictedNavigationItems(items, true)).toEqual(items)
  })

  test('does not resolve a persisted audit-log recent for a restricted user', () => {
    const recentUrls = [AUDIT_LOGS_URL]
    const navigationByUrl = buildAccessibleNavigationMap(
      [
        { title: 'Proxy logs', url: '/proxy-logs' },
        { title: 'Audit logs', url: AUDIT_LOGS_URL },
      ],
      false
    )
    const resolvedRecents = recentUrls.flatMap((url) => {
      const item = navigationByUrl.get(url)
      return item ? [item] : []
    })

    expect(resolvedRecents).toEqual([])
  })
})
