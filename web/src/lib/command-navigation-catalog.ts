// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Settings2 } from 'lucide-react'
import { AUDIT_LOGS_URL } from '@/components/dashboard/primary-navigation'
import { PLATFORM_SETTINGS_INDEX } from '@/lib/project-settings-index'
import {
  ASSISTANT_AREA,
  SERVER_ENTITIES,
  SERVER_HOME,
  SERVER_VIEWS,
  SETTINGS_AREA_GROUPS,
  type ServerNavArea,
} from '@/lib/server-navigation'
import {
  settingsNavigationGroups,
  type SettingsNavigationGroup,
  type SettingsNavigationIcon,
} from '@/components/settings/settings-navigation'

export interface IndexedNavigationItem {
  title: string
  url: string
  icon: SettingsNavigationIcon
  keywords?: string[]
}

export { AUDIT_LOGS_URL }

function areaItems(
  area: ServerNavArea,
  /** Where the area is listed, as one more word to find it by. */
  home?: string
): IndexedNavigationItem[] {
  const keywords = [
    area.description,
    ...(home ? [home] : []),
    ...(area.keywords ?? []),
  ]
  return [
    { title: area.title, url: area.url, icon: area.icon, keywords },
    // A tab that is not the area's landing page is a destination too, named
    // with its area so "Requests" or "Usage" says whose it is.
    ...(area.tabs ?? [])
      .filter((tab) => tab.url !== area.url)
      .map((tab) => ({
        title: `${area.title} · ${tab.title}`,
        url: tab.url,
        icon: tab.icon,
        keywords: [...keywords, tab.title],
      })),
  ]
}

/**
 * Every server-wide page, so the palette reaches each one by name whether
 * or not it is set up: the tree's branches, the tabs of the Projects page,
 * the pages listed in Settings and the assistant.
 */
export const serverNavigationItems: IndexedNavigationItem[] = [
  ...[SERVER_HOME, ...SERVER_VIEWS, ...SERVER_ENTITIES].flatMap((area) =>
    areaItems(area)
  ),
  ...SETTINGS_AREA_GROUPS.flatMap((group) =>
    group.areas.flatMap((area) => areaItems(area, `settings ${group.label}`))
  ),
  ...areaItems(ASSISTANT_AREA),
]

export function indexSettingsNavigationGroups(
  groups: readonly SettingsNavigationGroup[]
): IndexedNavigationItem[] {
  return groups.flatMap((group) =>
    group.items.flatMap((item) => {
      const keywords = [
        'settings',
        group.label,
        item.title,
        ...(item.keywords ?? []),
      ]
      return [
        { title: item.title, url: item.url, icon: item.icon, keywords },
        // A page behind an entry's tab is a destination of its own.
        ...(item.tabs ?? [])
          .filter((tab) => tab.url !== item.url)
          .map((tab) => ({
            title: `${item.title} · ${tab.title}`,
            url: tab.url,
            icon: tab.icon,
            keywords: [...keywords, tab.title],
          })),
      ]
    })
  )
}

export const settingsPageNavigationItems: IndexedNavigationItem[] =
  indexSettingsNavigationGroups(settingsNavigationGroups)

/** Individual platform controls, linked to their setting anchor. */
export const platformSettingNavigationItems: IndexedNavigationItem[] =
  PLATFORM_SETTINGS_INDEX.map((entry) => ({
    title: entry.title,
    url: `${entry.page}#${entry.anchor}`,
    icon: Settings2,
    keywords: ['setting', 'settings', ...entry.keywords],
  }))

/**
 * One result per URL. The first list to name a URL gives it its title and
 * icon; a later entry for the same URL adds its keywords, and its title as
 * one more, so a page is still found by what it used to be called.
 */
export function mergeNavigationItems(
  ...groups: IndexedNavigationItem[][]
): IndexedNavigationItem[] {
  const byUrl = new Map<string, IndexedNavigationItem>()
  for (const item of groups.flat()) {
    const existing = byUrl.get(item.url)
    if (!existing) {
      byUrl.set(item.url, item)
      continue
    }
    byUrl.set(item.url, {
      ...existing,
      keywords: [
        ...new Set([
          ...(existing.keywords ?? []),
          ...(item.title === existing.title ? [] : [item.title]),
          ...(item.keywords ?? []),
        ]),
      ],
    })
  }
  return [...byUrl.values()]
}

export function excludeNavigationUrls<T extends { url: string }>(
  items: readonly T[],
  excludedUrls: ReadonlySet<string>
): T[] {
  return items.filter((item) => !excludedUrls.has(item.url))
}

export function filterRestrictedNavigationItems<T extends { url: string }>(
  items: readonly T[],
  canViewAuditLogs: boolean
): T[] {
  return canViewAuditLogs
    ? [...items]
    : items.filter((item) => item.url !== AUDIT_LOGS_URL)
}

export function buildAccessibleNavigationMap<T extends { url: string }>(
  items: readonly T[],
  canViewAuditLogs: boolean
): Map<string, T> {
  return new Map(
    filterRestrictedNavigationItems(items, canViewAuditLogs).map((item) => [
      item.url,
      item,
    ])
  )
}
