// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  PLUGINS_MANAGEMENT_URL,
  isPluginsManagementRoute,
  isSettingsSidebarRoute,
  primaryPlatformGroups,
  type PlatformNavItem,
} from './primary-navigation'

const items = primaryPlatformGroups.flatMap((group) => group.items)
const find = (title: string): PlatformNavItem => {
  const item = items.find((entry) => entry.title === title)
  if (!item) throw new Error(`sidebar entry "${title}" is missing`)
  return item
}
const groupOf = (title: string) =>
  primaryPlatformGroups.find((group) =>
    group.items.some((entry) => entry.title === title)
  )?.label

// Mirrors NavSection: activeWhen wins, otherwise the exact url.
const isActive = (item: PlatformNavItem, pathname: string) =>
  item.activeWhen?.(pathname) ?? item.url === pathname

const activeTitles = (pathname: string) =>
  items.filter((item) => isActive(item, pathname)).map((item) => item.title)

describe('primary sidebar entries', () => {
  test('Email and Plugins are always listed, with no gating', () => {
    expect(find('Email').url).toBe('/email')
    expect(find('Plugins').url).toBe('/settings/plugins')
    expect(find('Plugins').url).toBe(PLUGINS_MANAGEMENT_URL)
  })

  test('places Email with delivery and Plugins beside All tools', () => {
    expect(groupOf('Email')).toBe('Applications')
    expect(groupOf('Plugins')).toBe('Administration')
    expect(groupOf('All tools')).toBe('Administration')
  })

  test('has unique urls and titles', () => {
    expect(new Set(items.map((i) => i.url)).size).toBe(items.length)
    expect(new Set(items.map((i) => i.title)).size).toBe(items.length)
  })
})

describe('Email active state', () => {
  test.each([
    '/email',
    '/email/12',
    '/email/domains/new',
    '/email/domains/4',
    '/email/providers/new',
    '/email/providers/2',
  ])('highlights only Email on %s', (path) => {
    expect(activeTitles(path)).toEqual(['Email'])
  })

  test('does not match a similar prefix', () => {
    expect(isActive(find('Email'), '/emails')).toBe(false)
  })
})

describe('Plugins active state', () => {
  test.each(['/settings/plugins', '/settings/plugins/install'])(
    'highlights only Plugins on %s',
    (path) => {
      expect(activeTitles(path)).toEqual(['Plugins'])
    }
  )

  test('does not highlight the management entry on installed plugin pages', () => {
    expect(activeTitles('/plugins/some-plugin')).toEqual([])
    expect(activeTitles('/plugins/install')).toEqual([])
    expect(isPluginsManagementRoute('/plugins/some-plugin/settings')).toBe(
      false
    )
  })

  test('does not match a similar prefix', () => {
    expect(isPluginsManagementRoute('/settings/plugins-extra')).toBe(false)
  })
})

describe('settings sidebar swap', () => {
  test('keeps the main sidebar on Plugins and Worker nodes', () => {
    expect(isSettingsSidebarRoute('/settings/plugins')).toBe(false)
    expect(isSettingsSidebarRoute('/settings/plugins/install')).toBe(false)
    expect(isSettingsSidebarRoute('/settings/nodes')).toBe(false)
  })

  test('still swaps for other settings pages', () => {
    expect(isSettingsSidebarRoute('/settings')).toBe(true)
    expect(isSettingsSidebarRoute('/settings/security')).toBe(true)
    expect(isSettingsSidebarRoute('/settings/plugins-extra')).toBe(true)
    expect(isSettingsSidebarRoute('/email')).toBe(false)
  })
})
