// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { platformToolGroups } from '@/components/platform/platform-tools'
import { settingsNavigationGroups } from '@/components/settings/settings-navigation'
import type { LucideIcon } from 'lucide-react'

export interface IndexedNavigationItem {
  title: string
  url: string
  icon: LucideIcon
  keywords?: string[]
}

export const platformToolNavigationItems: IndexedNavigationItem[] =
  platformToolGroups.flatMap((group) =>
    group.items.map((item) => ({
      title: item.title,
      url: item.url,
      icon: item.icon,
      keywords: [
        group.label,
        group.description,
        item.description,
        ...(item.keywords ?? []),
      ],
    }))
  )

export const settingsPageNavigationItems: IndexedNavigationItem[] =
  settingsNavigationGroups.flatMap((group) =>
    group.items.map((item) => ({
      title: item.title,
      url: item.url,
      icon: item.icon,
      keywords: ['settings', group.label, item.title],
    }))
  )

/**
 * Prefer purpose-written command labels while merging the canonical page
 * registry behind them. New canonical pages are appended automatically and
 * duplicate URLs retain keywords from both sources.
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
        ...new Set([...(existing.keywords ?? []), ...(item.keywords ?? [])]),
      ],
    })
  }
  return [...byUrl.values()]
}
