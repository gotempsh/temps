// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { platformToolGroups } from '@/components/platform/platform-tools'
import { settingsNavigationGroups } from '@/components/settings/settings-navigation'
import {
  AUDIT_LOGS_URL,
  buildAccessibleNavigationMap,
  excludeNavigationUrls,
  filterRestrictedNavigationItems,
  isSettingsNavigationUrl,
  mergeNavigationItems,
  platformToolNavigationItems,
  settingsPageNavigationItems,
} from './command-navigation-catalog'

describe('command navigation catalog', () => {
  test('indexes every platform tool from the page registry', () => {
    const expectedUrls = platformToolGroups.flatMap((group) =>
      group.items.map((item) => item.url)
    )
    const indexedUrls = new Set(
      platformToolNavigationItems.map((item) => item.url)
    )

    expect(expectedUrls.every((url) => indexedUrls.has(url))).toBe(true)
    expect(indexedUrls.has('/certificates')).toBe(true)
  })

  test('indexes every page rendered by the settings sidebar', () => {
    const expectedUrls = settingsNavigationGroups.flatMap((group) =>
      group.items.map((item) => item.url)
    )
    const indexedUrls = new Set(
      settingsPageNavigationItems.map((item) => item.url)
    )

    expect(expectedUrls.every((url) => indexedUrls.has(url))).toBe(true)
    for (const url of [
      '/settings/version',
      '/settings/request-timeouts',
      '/settings/traefik-discovery',
      '/settings/mcp-server',
      '/settings/otel-pipeline',
    ]) {
      expect(indexedUrls.has(url)).toBe(true)
    }
  })

  test('keeps one result per URL and combines search keywords', () => {
    const [canonical] = settingsPageNavigationItems.filter(
      (item) => item.url === '/settings'
    )
    const results = mergeNavigationItems(
      [
        {
          ...canonical,
          title: 'Platform Settings',
          keywords: ['configuration'],
        },
      ],
      [canonical]
    )
    const [result] = results

    expect(results).toHaveLength(1)
    expect(new Set(results.map((item) => item.url)).size).toBe(results.length)
    expect(result.title).toBe('Platform Settings')
    expect(result.keywords).toContain('configuration')
    expect(result.keywords).toContain('settings')
  })

  test('keeps settings pages out of the main navigation category', () => {
    const settingsUrls = new Set(
      settingsPageNavigationItems.map((item) => item.url)
    )
    const mainItems = excludeNavigationUrls(
      platformToolNavigationItems,
      settingsUrls
    )

    expect(mainItems.some((item) => item.url === '/settings')).toBe(false)
    expect(mainItems.some((item) => item.url === '/settings/mcp-server')).toBe(
      false
    )
    expect(mainItems.some((item) => item.url === '/projects')).toBe(true)
    expect(isSettingsNavigationUrl('/settings')).toBe(true)
    expect(isSettingsNavigationUrl('/settings/mcp-server')).toBe(true)
    expect(isSettingsNavigationUrl('/storage')).toBe(false)
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
