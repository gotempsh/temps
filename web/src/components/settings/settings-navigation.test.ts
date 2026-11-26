// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  mergeSettingsNavigationGroups,
  resolveSettingsTab,
  settingsNavigationGroups,
} from './settings-navigation'
import { WORKER_NODES_URL } from '@/lib/worker-nodes'
import { ON_DEMAND_TLS_SETTINGS_PATH } from '@/lib/on-demand-tls'

const labels = (groups: ReturnType<typeof mergeSettingsNavigationGroups>) =>
  groups.map((g) => g.label)

describe('settingsNavigationGroups', () => {
  const items = settingsNavigationGroups.flatMap((g) => g.items)
  const urls = items.map((i) => i.url)

  it('groups settings by what they govern', () => {
    expect(labels(settingsNavigationGroups)).toEqual([
      // What the platform connects to and what agents may use come first.
      'Platform',
      'AI',
      'Access',
      'Security',
      'Instance',
      'Data',
    ])
    const group = (title: string) =>
      settingsNavigationGroups.find((g) =>
        g.items.some((i) => i.title === title)
      )?.label
    expect(group('Audit log')).toBe('Access')
    expect(group('Sign-in')).toBe('Access')
    expect(group('Limits')).toBe('Instance')
    expect(group('Extensions')).toBe('Data')
  })

  it('does not list the server areas that keep a /settings URL', () => {
    // Servers, notification channels and the MCP server are areas of the
    // server menu. Listing them here too would give them two homes and swap
    // the sidebar away from the entry that is active.
    expect(urls).not.toContain(WORKER_NODES_URL)
    expect(urls).not.toContain('/settings/notifications')
    expect(urls).not.toContain('/settings/mcp-server')
  })

  it('lists each page once, under one name', () => {
    const all = items.flatMap((i) => [
      ...new Set([i.url, ...(i.tabs ?? []).map((t) => t.url)]),
    ])
    expect(new Set(all).size).toBe(all.length)
    expect(new Set(items.map((i) => i.title)).size).toBe(items.length)
  })

  it('says what every built-in page is for', () => {
    for (const item of items)
      expect(item.description?.length).toBeGreaterThan(0)
  })

  it('folds the three limit pages into one entry with tabs', () => {
    const limits = items.find((i) => i.title === 'Limits')
    expect(limits?.tabs?.map((t) => t.url)).toEqual([
      '/settings/build-limits',
      '/settings/request-timeouts',
      '/settings/service-healthchecks',
    ])
    expect(limits?.url).toBe(limits?.tabs?.[0].url)
    for (const url of ['/settings/request-timeouts', '/settings/build-limits'])
      expect(resolveSettingsTab(url)?.item.title).toBe('Limits')
    expect(resolveSettingsTab('/settings/request-timeouts')?.tab.title).toBe(
      'Request timeouts'
    )
    expect(resolveSettingsTab('/settings/users')).toBeUndefined()
    expect(resolveSettingsTab('/settings/build-limits/x')).toBeUndefined()
  })

  it('lists the on-demand TLS page the Certificates empty state links to', () => {
    const item = settingsNavigationGroups
      .flatMap((g) => g.items)
      .find((i) => i.url === ON_DEMAND_TLS_SETTINGS_PATH)
    expect(item?.title).toBe('On-demand TLS')
    expect(item?.keywords).toContain('certificates')
  })
})

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
      id: 'sso',
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
