// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  mergeSettingsNavigationGroups,
  settingsNavigationGroups,
} from './settings-navigation'

const labels = (groups: ReturnType<typeof mergeSettingsNavigationGroups>) =>
  groups.map((g) => g.label)

describe('mergeSettingsNavigationGroups', () => {
  it('returns the built-in groups untouched when there are no extensions', () => {
    const out = mergeSettingsNavigationGroups(
      settingsNavigationGroups,
      undefined
    )
    expect(labels(out)).toEqual(labels(settingsNavigationGroups))
    expect(out.flatMap((g) => g.items.length)).toEqual(
      settingsNavigationGroups.map((g) => g.items.length)
    )
  })

  it('appends to a built-in group after its own entries', () => {
    const out = mergeSettingsNavigationGroups(settingsNavigationGroups, [
      {
        id: 'sso',
        label: 'Single Sign-On',
        path: '/settings/sso',
        group: 'Access',
      },
    ])
    const access = out.find((g) => g.label === 'Access')!
    const original = settingsNavigationGroups.find((g) => g.label === 'Access')!
    expect(access.items.slice(0, original.items.length)).toEqual(original.items)
    expect(access.items[access.items.length - 1]).toMatchObject({
      title: 'Single Sign-On',
      url: '/settings/sso',
    })
    expect(original.items.some((i) => i.url === '/settings/sso')).toBe(false)
  })

  it('creates unknown groups after the built-in ones, in first-seen order', () => {
    const out = mergeSettingsNavigationGroups(settingsNavigationGroups, [
      { id: 'a', label: 'A', path: '/settings/a', group: 'Compliance' },
      { id: 'b', label: 'B', path: '/settings/b', group: 'Appearance' },
      { id: 'c', label: 'C', path: '/settings/c', group: 'Compliance' },
    ])
    expect(labels(out)).toEqual([
      ...labels(settingsNavigationGroups),
      'Compliance',
      'Appearance',
    ])
    expect(out[out.length - 2].items.map((i) => i.title)).toEqual(['A', 'C'])
  })

  it('drops items that do not live under /settings/', () => {
    const out = mergeSettingsNavigationGroups(settingsNavigationGroups, [
      { id: 'x', label: 'Elsewhere', path: '/ee/elsewhere', group: 'Access' },
    ])
    expect(
      out.flatMap((g) => g.items).some((i) => i.title === 'Elsewhere')
    ).toBe(false)
  })

  it('hands back a stable icon component for the same node', () => {
    const icon = { type: 'svg' } as unknown as import('react').ReactNode
    const item = {
      id: 'x',
      label: 'X',
      path: '/settings/x',
      group: 'Access',
      icon,
    }
    const first = mergeSettingsNavigationGroups(settingsNavigationGroups, [
      item,
    ])
    const second = mergeSettingsNavigationGroups(settingsNavigationGroups, [
      item,
    ])
    const pick = (g: typeof first) =>
      g.find((x) => x.label === 'Access')!.items.slice(-1)[0].icon
    expect(pick(first)).toBe(pick(second))
  })
})
