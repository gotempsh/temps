// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { platformToolGroups } from '@/components/platform/platform-tools'
import { settingsNavigationGroups } from '@/components/settings/settings-navigation'
import {
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
    const [result] = mergeNavigationItems(
      [
        {
          ...canonical,
          title: 'Platform Settings',
          keywords: ['configuration'],
        },
      ],
      [canonical]
    )

    expect(result.title).toBe('Platform Settings')
    expect(result.keywords).toContain('configuration')
    expect(result.keywords).toContain('settings')
  })
})
